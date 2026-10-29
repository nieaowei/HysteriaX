use super::*;
use std::collections::BTreeMap;

#[derive(Clone)]
pub(super) struct Password {
    pub(super) plaintext: String,
    pub(super) digest: String,
    pub(super) ciphertext: String,
}

#[derive(Clone)]
pub(super) struct PreparedAssignments {
    entries: BTreeMap<Pair, Password>,
    pub(super) timestamp: DateTime<Utc>,
}

impl PreparedAssignments {
    pub(super) fn get(&self, user: &str, node: &str) -> Result<&Password, ApiError> {
        self.entries
            .get(&(user.to_owned(), node.to_owned()))
            .ok_or_else(|| {
                ApiError::conflict(
                    "authorization assignments changed during preparation; retry the change",
                )
            })
    }
}

async fn missing_on(
    tx: &mut Transaction<'_, Postgres>,
    users: &[String],
    after: &HashSet<Pair>,
) -> Result<(Vec<Pair>, DateTime<Utc>), ApiError> {
    let materialized:HashSet<Pair>=sqlx::query_as("SELECT user_id,node_id FROM node_assignments WHERE user_id=ANY($1) ORDER BY user_id,node_id")
        .bind(users).fetch_all(&mut **tx).await?.into_iter().collect();
    let timestamp = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut **tx)
        .await?;
    Ok((pair_diff(&materialized, after).0, timestamp))
}

fn prepare(
    state: &AppState,
    missing: Vec<Pair>,
    timestamp: DateTime<Utc>,
) -> Result<PreparedAssignments, ApiError> {
    let mut entries = BTreeMap::new();
    for pair in missing {
        let plaintext = generate_token();
        entries.insert(
            pair,
            Password {
                digest: token_digest(&plaintext),
                ciphertext: state.secrets.encrypt(&plaintext)?,
                plaintext,
            },
        );
    }
    Ok(PreparedAssignments { entries, timestamp })
}

pub(super) async fn prepare_group_change(
    state: &AppState,
    action: &str,
    group_id: Option<&str>,
    proposal: &GroupProposal,
) -> Result<
    (
        crate::credentials::authorization::Prepared,
        PreparedAssignments,
    ),
    ApiError,
> {
    let mut tx = crate::read_snapshot::transaction(&state.pool).await?;
    let mut users = proposal.user_ids.clone();
    if let Some(id) = group_id {
        let current = load_group_tx(&mut tx, id)
            .await?
            .ok_or_else(|| ApiError::not_found("authorization group"))?;
        users.extend(current.user_ids);
    }
    if action != "delete" {
        validate_proposal_entities(&mut tx, proposal).await?;
    }
    sort_dedup(&mut users);
    let before = effective_pairs(&mut tx, &users, None).await?;
    let mut after = if action == "create" {
        before.clone()
    } else {
        effective_pairs(&mut tx, &users, group_id).await?
    };
    if action != "delete" {
        add_group_pairs(&mut after, &proposal.user_ids, &proposal.node_ids);
    }
    let (additions, _) = pair_diff(&before, &after);
    let plan = crate::credentials::authorization::plan_on(&mut tx, &additions).await?;
    let (missing, timestamp) = missing_on(&mut tx, &users, &after).await?;
    tx.commit().await?;
    let managed =
        crate::credentials::authorization::prepare_plan(&state.pool, &state.secrets, plan)
            .await
            .map_err(authorization_transaction_error)?;
    Ok((managed, prepare(state, missing, timestamp)?))
}

pub(super) async fn prepare_membership_change(
    state: &AppState,
    user: &str,
    proposal: &MembershipProposal,
) -> Result<
    (
        crate::credentials::authorization::Prepared,
        PreparedAssignments,
    ),
    ApiError,
> {
    let mut tx = crate::read_snapshot::transaction(&state.pool).await?;
    user_revision(&mut tx, user).await?;
    validate_group_ids(&mut tx, &proposal.group_ids).await?;
    let before = effective_pairs(&mut tx, &[user.to_owned()], None).await?;
    let after = pairs_for_user_groups(&mut tx, user, &proposal.group_ids).await?;
    let (additions, _) = pair_diff(&before, &after);
    let plan = crate::credentials::authorization::plan_on(&mut tx, &additions).await?;
    let (missing, timestamp) = missing_on(&mut tx, &[user.to_owned()], &after).await?;
    tx.commit().await?;
    let managed =
        crate::credentials::authorization::prepare_plan(&state.pool, &state.secrets, plan)
            .await
            .map_err(authorization_transaction_error)?;
    Ok((managed, prepare(state, missing, timestamp)?))
}

#[cfg(test)]
pub(super) async fn prepare_group_assignments(
    state: &AppState,
    action: &str,
    group_id: Option<&str>,
    proposal: &GroupProposal,
) -> Result<PreparedAssignments, ApiError> {
    // This snapshot prepares candidates only. The final write still checks the
    // version, identity plan and complete authoritative graph after resource locking.
    let mut tx = crate::read_snapshot::transaction(&state.pool).await?;
    let mut users = proposal.user_ids.clone();
    if let Some(id) = group_id {
        users.extend(
            load_group_tx(&mut tx, id)
                .await?
                .ok_or_else(|| ApiError::not_found("authorization group"))?
                .user_ids,
        );
    }
    sort_dedup(&mut users);
    let mut after = effective_pairs(
        &mut tx,
        &users,
        if action == "create" { None } else { group_id },
    )
    .await?;
    if action != "delete" {
        add_group_pairs(&mut after, &proposal.user_ids, &proposal.node_ids);
    }
    let (missing, timestamp) = missing_on(&mut tx, &users, &after).await?;
    tx.commit().await?;
    prepare(state, missing, timestamp)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn candidate(
        state: &AppState,
    ) -> (
        GroupProposal,
        crate::credentials::authorization::Prepared,
        PreparedAssignments,
    ) {
        let proposal = GroupProposal {
            name: "Prepared".into(),
            user_ids: vec!["user".into()],
            node_ids: vec!["node".into()],
        };
        let (managed, assignments) = prepare_group_change(state, "create", None, &proposal)
            .await
            .unwrap();
        (proposal, managed, assignments)
    }

    #[tokio::test]
    async fn native_retries_preserve_the_prepared_group_id_password_digest_ciphertext_and_timestamp()
     {
        for code in ["40001", "40P01"] {
            let state = super::super::tests::fixture().await;
            sqlx::query("DELETE FROM node_assignments")
                .execute(&state.pool)
                .await
                .unwrap();
            let (proposal, managed, assignments) = candidate(&state).await;
            let material = assignments.get("user", "node").unwrap().clone();
            let timestamp = assignments.timestamp;
            let unchanged:i64=sqlx::query_scalar("SELECT (SELECT count(*) FROM authorization_groups)+(SELECT count(*) FROM node_assignments)").fetch_one(&state.pool).await.unwrap();
            assert_eq!(unchanged, 0);
            let id = Uuid::new_v4().to_string();
            let expected = id.clone();
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SEQUENCE grant_attempt; CREATE FUNCTION retry_prepared_grant() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='authorization_group.created' AND nextval('grant_attempt')=1 THEN RAISE EXCEPTION 'retry' USING ERRCODE='{code}'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_prepared_grant BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION retry_prepared_grant()")))
                .execute(&state.pool).await.unwrap();
            let app = state.clone();
            let receipt = db::serializable(&state.pool, move |tx| {
                let (app, proposal, managed, assignments, id) = (
                    app.clone(),
                    proposal.clone(),
                    managed.clone(),
                    assignments.clone(),
                    id.clone(),
                );
                Box::pin(async move {
                    super::super::commit_group_on(
                        tx,
                        &app,
                        "create",
                        None,
                        None,
                        proposal,
                        managed,
                        &id,
                        assignments,
                    )
                    .await
                })
            })
            .await
            .unwrap();
            assert_eq!(receipt, json!({"revocation_job_ids":[]}));
            let stored_id: String = sqlx::query_scalar("SELECT id FROM authorization_groups")
                .fetch_one(&state.pool)
                .await
                .unwrap();
            assert_eq!(stored_id, expected);
            let row=sqlx::query("SELECT credential_hash,credential_enc,created_at FROM node_assignments WHERE user_id='user' AND node_id='node'").fetch_one(&state.pool).await.unwrap();
            assert_eq!(row.get::<String, _>("credential_hash"), material.digest);
            assert_eq!(row.get::<String, _>("credential_enc"), material.ciphertext);
            assert_eq!(row.get::<DateTime<Utc>, _>("created_at"), timestamp);
            let attempts: i64 = sqlx::query_scalar("SELECT last_value FROM grant_attempt")
                .fetch_one(&state.pool)
                .await
                .unwrap();
            assert_eq!(attempts, 2);
        }
    }

    #[tokio::test]
    async fn an_assignment_removed_after_preparation_cannot_be_recreated_without_a_candidate() {
        let state = super::super::tests::fixture().await;
        sqlx::query("INSERT INTO node_assignments(user_id,node_id,credential_hash,credential_enc,created_at) VALUES('user','node',$1,$2,clock_timestamp()) ON CONFLICT DO NOTHING")
            .bind(token_digest("before-preparation")).bind(state.secrets.encrypt("before-preparation").unwrap()).execute(&state.pool).await.unwrap();
        let (proposal, managed, assignments) = candidate(&state).await;
        assert!(assignments.entries.is_empty());
        sqlx::query("DELETE FROM node_assignments")
            .execute(&state.pool)
            .await
            .unwrap();
        let app = state.clone();
        let id = Uuid::new_v4().to_string();
        let error = db::serializable(&state.pool, move |tx| {
            let (app, proposal, managed, assignments, id) = (
                app.clone(),
                proposal.clone(),
                managed.clone(),
                assignments.clone(),
                id.clone(),
            );
            Box::pin(async move {
                super::super::commit_group_on(
                    tx,
                    &app,
                    "create",
                    None,
                    None,
                    proposal,
                    managed,
                    &id,
                    assignments,
                )
                .await
            })
        })
        .await
        .unwrap_err();
        assert_eq!(error.status, StatusCode::CONFLICT);
        let unchanged:i64=sqlx::query_scalar("SELECT (SELECT count(*) FROM authorization_groups)+(SELECT count(*) FROM node_assignments)+(SELECT count(*) FROM audit_records WHERE action='authorization_group.created')").fetch_one(&state.pool).await.unwrap();
        assert_eq!(unchanged, 0);
    }
}
