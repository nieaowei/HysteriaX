//! Bounded identity signing scopes nested inside a claimed deployment job.
use super::*;
use anyhow::{Context, Result, ensure};
use std::collections::BTreeSet;

#[derive(Debug)]
struct Busy;
impl std::fmt::Display for Busy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("shared identity is busy")
    }
}
impl std::error::Error for Busy {}

// Acquisition/release must include the inherited resources in lexical order.
// The ordinary retry boundary would lock the inherited job/node first; use a
// private empty scope only around this closed database operation, then explicitly
// validate the complete original scope before committing. No caller future is
// ever polled in this empty scope.
fn acquisition_scope(parent: &Scope) -> Scope {
    Scope {
        pool: parent.pool.clone(),
        leases: vec![],
        sequence: parent.sequence.clone(),
    }
}

pub(crate) async fn with_identity_leases<T>(
    identities: &[String],
    work: impl std::future::Future<Output = Result<T>>,
) -> Result<Option<T>> {
    ensure!(
        (1..=100).contains(&identities.len()),
        "identity signing scope must contain 1 to 100 identities"
    );
    ensure!(
        identities
            .iter()
            .all(|id| !id.is_empty() && id.len() <= 200),
        "invalid shared identity key"
    );
    let parent = CURRENT
        .try_with(Clone::clone)
        .context("shared signing requires a job execution scope")?;
    let primary = parent
        .leases
        .iter()
        .find(|l| l.resource.starts_with("job:"))
        .context("shared signing requires its job lease")?
        .clone();
    ensure!(
        parent
            .leases
            .iter()
            .all(|l| l.work == primary.work && l.owner == primary.owner),
        "mixed signing owner scope"
    );
    ensure!(
        parent
            .leases
            .iter()
            .any(|l| l.resource.starts_with("node:")),
        "shared signing requires its node lease"
    );
    let keys: BTreeSet<String> = parent
        .leases
        .iter()
        .map(|l| l.resource.clone())
        .chain(identities.iter().map(|id| format!("identity:{id}")))
        .collect();
    let original = parent.clone();
    let acquired = CURRENT
        .scope(
            acquisition_scope(&parent),
            crate::db::serializable::<_, anyhow::Error, _>(&parent.pool, move |tx| {
                let (original, primary, keys) = (original.clone(), primary.clone(), keys.clone());
                Box::pin(async move {
                    let mut leases = vec![];
                    for key in keys {
                        if let Some(lease) = original.leases.iter().find(|l| l.resource == key) {
                            lease.guard(tx).await?;
                            leases.push(lease.clone());
                        } else {
                            leases.push(
                                acquire(tx, &key, &primary.owner, &primary.work)
                                    .await?
                                    .ok_or(Busy)?,
                            );
                        }
                    }
                    CURRENT.scope(original, guard_current(tx)).await?;
                    Ok(leases)
                })
            }),
        )
        .await;
    let leases = match acquired {
        Ok(leases) => leases,
        Err(error) if error.is::<Busy>() => return Ok(None),
        Err(error) => return Err(error),
    };
    let child = Scope {
        pool: parent.pool.clone(),
        leases: leases.clone(),
        sequence: parent.sequence.clone(),
    };
    let result = supervise_scope(child.clone(), work).await;
    // Loss of any lease cancels the future. Never release a replacement owner's
    // resources; untouched expired identity leases remain reclaimable normally.
    let Some(result) = result else {
        return Ok(None);
    };
    let inherited: BTreeSet<String> = parent.leases.iter().map(|l| l.resource.clone()).collect();
    CURRENT
        .scope(
            acquisition_scope(&parent),
            crate::db::serializable::<_, anyhow::Error, _>(&parent.pool, move |tx| {
                let (leases, child, inherited) = (leases.clone(), child.clone(), inherited.clone());
                Box::pin(async move {
                    for lease in &leases {
                        lease.guard(tx).await?;
                    }
                    CURRENT.scope(child, guard_current(tx)).await?;
                    for lease in &leases {
                        if !inherited.contains(&lease.resource) {
                            ensure!(lease.release(tx).await?, "shared identity became uncertain");
                        }
                    }
                    Ok(())
                })
            }),
        )
        .await?;
    result.map(Some)
}
