//! Durable, cursor-based authorization changes. No graph expansion in HTTP handlers.
use super::*;
use crate::{
    db,
    deployment::{JobInput, JobOutput},
    security::SecretBox,
};
use anyhow::{Context, Result, ensure};
use sqlx::PgPool;

const PAGE: i64 = 256;
fn ids(input: &Value, key: &str) -> Vec<String> {
    input[key]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

pub(super) async fn admit(
    tx: &mut Transaction<'_, Postgres>,
    target_type: &str,
    target: &str,
    action: &str,
    mut input: Value,
    actor: &str,
) -> Result<Value, ApiError> {
    let mut resource_name = input["name"].as_str().unwrap_or(target).to_owned();
    if target_type == "group" && action == "create" {
        sqlx::query("INSERT INTO authorization_groups(id,name,revision,created_at,updated_at) VALUES($1,$2,1,now(),now())")
            .bind(target).bind(input["name"].as_str()).execute(&mut **tx).await?;
    } else {
        let query = if target_type == "group" {
            "SELECT revision,name FROM authorization_groups WHERE id=$1 AND NOT deleting FOR UPDATE"
        } else {
            "SELECT revision,name FROM users WHERE id=$1 FOR NO KEY UPDATE"
        };
        let current = sqlx::query(query)
            .bind(target)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or_else(|| ApiError::not_found(target_type))?;
        check_revision(current.get("revision"), input["expected_revision"].as_i64())?;
        resource_name = input["name"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| current.get("name"));
    }
    let pending:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM authorization_changes WHERE target_type=$1 AND target_id=$2 AND NOT complete)")
        .bind(target_type).bind(target).fetch_one(&mut **tx).await?;
    if pending {
        return Err(ApiError::conflict(
            "an authorization change is pending; open its job before editing again",
        ));
    }
    if target_type == "user" {
        let groups = ids(&input, "group_ids");
        validate_group_ids(tx, &groups).await?;
        let removed:Vec<String>=sqlx::query_scalar("SELECT group_id FROM authorization_group_users WHERE user_id=$1 AND NOT(group_id=ANY($2)) ORDER BY group_id")
            .bind(target).bind(&groups).fetch_all(&mut **tx).await?;
        input["remove_group_ids"] = json!(removed);
    } else {
        validate_ids_exist(tx, "users", &ids(&input, "add_user_ids"), "user").await?;
        validate_ids_exist(tx, "nodes", &ids(&input, "add_node_ids"), "node").await?;
    }
    // Persist actual deltas: repeated additions must not walk a large group.
    if target_type == "group" {
        for (key, table, column, remove) in [
            (
                "add_user_ids",
                "authorization_group_users",
                "user_id",
                false,
            ),
            (
                "remove_user_ids",
                "authorization_group_users",
                "user_id",
                true,
            ),
            (
                "add_node_ids",
                "authorization_group_nodes",
                "node_id",
                false,
            ),
            (
                "remove_node_ids",
                "authorization_group_nodes",
                "node_id",
                true,
            ),
        ] {
            let candidates = ids(&input, key);
            let query = format!(
                "SELECT item FROM unnest($2::text[]) item WHERE EXISTS(SELECT 1 FROM {table} WHERE group_id=$1 AND {column}=item)=$3 ORDER BY item"
            );
            let actual: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(query))
                .bind(target)
                .bind(candidates)
                .bind(remove)
                .fetch_all(&mut **tx)
                .await?;
            input[key] = json!(actual);
        }
    }
    let overlap: bool = if target_type == "user" {
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM authorization_changes c WHERE c.target_type='group' AND NOT c.complete AND (c.target_id=ANY($2) OR EXISTS(SELECT 1 FROM authorization_group_users gu WHERE gu.group_id=c.target_id AND gu.user_id=$1)))")
            .bind(target).bind(ids(&input,"group_ids")).fetch_one(&mut **tx).await?
    } else {
        let mut changed = ids(&input, "add_user_ids");
        changed.extend(ids(&input, "remove_user_ids"));
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM authorization_changes c WHERE c.target_type='user' AND NOT c.complete AND (c.target_id=ANY($2) OR EXISTS(SELECT 1 FROM authorization_group_users gu WHERE gu.group_id=$1 AND gu.user_id=c.target_id) OR EXISTS(SELECT 1 FROM authorization_change_items ci WHERE ci.job_id=c.job_id AND ci.kind='group_ids' AND ci.item_id=$1)))")
            .bind(target).bind(changed).fetch_one(&mut **tx).await?
    };
    if overlap {
        return Err(ApiError::conflict(
            "a related membership change is pending; wait for its authorization job",
        ));
    }
    if target_type == "user" {
        let actual:Vec<String>=sqlx::query_scalar("SELECT item FROM unnest($2::text[]) item WHERE NOT EXISTS(SELECT 1 FROM authorization_group_users WHERE user_id=$1 AND group_id=item) ORDER BY item")
            .bind(target).bind(ids(&input,"group_ids")).fetch_all(&mut **tx).await?;
        input["group_ids"] = json!(actual);
    }
    let job = super::super::enqueue_job_with_payload_in_tx(
        tx,
        "authorization-change",
        None,
        None,
        json!({}),
    )
    .await?;
    sqlx::query("UPDATE jobs SET payload_json=jsonb_build_object('authorization_change_id',id),resource_key=$2,resource_type=$3,resource_id=$4,resource_name=$5 WHERE id=$1")
        .bind(&job).bind(format!("authorization:{target_type}:{target}")).bind(if target_type=="group" {"authorization_group"} else {"user"}).bind(target)
        .bind(resource_name).execute(&mut **tx).await?;
    let mut metadata = input.clone();
    for key in [
        "add_user_ids",
        "remove_user_ids",
        "add_node_ids",
        "remove_node_ids",
        "group_ids",
        "remove_group_ids",
    ] {
        metadata.as_object_mut().unwrap().remove(key);
    }
    sqlx::query("INSERT INTO authorization_changes(job_id,target_type,target_id,action,actor_id,input) VALUES($1,$2,$3,$4,$5,$6)")
        .bind(&job).bind(target_type).bind(target).bind(action).bind(actor).bind(metadata).execute(&mut **tx).await?;
    sqlx::query("INSERT INTO authorization_change_items(job_id,kind,item_id) SELECT $1,x.key,v #>> '{}' FROM jsonb_each($2) x CROSS JOIN LATERAL jsonb_array_elements(CASE WHEN jsonb_typeof(x.value)='array' THEN x.value ELSE '[]'::jsonb END) v WHERE x.key=ANY(ARRAY['add_user_ids','remove_user_ids','add_node_ids','remove_node_ids','group_ids','remove_group_ids']) ON CONFLICT DO NOTHING")
        .bind(&job).bind(&input).execute(&mut **tx).await?;
    if action == "delete" {
        sqlx::query("INSERT INTO authorization_exclusions(job_id,group_id) VALUES($1,$2)")
            .bind(&job)
            .bind(target)
            .execute(&mut **tx)
            .await?;
    } else if target_type == "group" {
        sqlx::query("INSERT INTO authorization_exclusions(job_id,group_id,user_id,node_id) SELECT $1,$2,u,'' FROM unnest($3::text[])u UNION ALL SELECT $1,$2,'',n FROM unnest($4::text[])n")
            .bind(&job).bind(target).bind(ids(&input,"remove_user_ids")).bind(ids(&input,"remove_node_ids")).execute(&mut **tx).await?;
    } else {
        sqlx::query("INSERT INTO authorization_exclusions(job_id,group_id,user_id) SELECT $1,g,$2 FROM unnest($3::text[])g")
            .bind(&job).bind(target).bind(ids(&input,"remove_group_ids")).execute(&mut **tx).await?;
    }
    if action != "create" {
        let query = if target_type == "group" {
            "UPDATE authorization_groups SET revision=revision+1,updated_at=now() WHERE id=$1"
        } else {
            "UPDATE users SET revision=revision+1,updated_at=now() WHERE id=$1"
        };
        sqlx::query(query).bind(target).execute(&mut **tx).await?;
    }
    super::super::audit_on(
        tx,
        actor,
        "authorization_change.queued",
        if target_type == "group" {
            "authorization_group"
        } else {
            "user"
        },
        target,
        json!({"job_id":job,"action":action}),
    )
    .await?;
    Ok(json!({"job_id":job,"resource_id":target,"status":"queued"}))
}

pub(crate) async fn run(pool: &PgPool, _secrets: &SecretBox, job: &JobInput) -> Result<JobOutput> {
    let change = job.payload["authorization_change_id"]
        .as_str()
        .context("missing authorization change")?;
    loop {
        let job = job.clone();
        let change = change.to_owned();
        let done = db::serializable::<_, anyhow::Error, _>(pool, move |tx| {
            let (job, change) = (job.clone(), change.clone());
            Box::pin(async move { Box::pin(page_on(tx, &job, &change)).await })
        })
        .await?;
        if let Some(result) = done {
            return Ok(JobOutput {
                stage: "authorization_applied".into(),
                result,
                node_state: None,
                deployed_revision: None,
                deployed_config: None,
                deployed_sha256: None,
                published_connection: None,
                delete_node: false,
            });
        }
        tokio::task::yield_now().await;
    }
}

async fn page_on(
    tx: &mut Transaction<'static, Postgres>,
    job: &JobInput,
    change: &str,
) -> Result<Option<Value>> {
    crate::execution::require_job_scope(&job.id, None)?;
    let active:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM jobs WHERE id=$1 AND kind='authorization-change' AND status='running' AND attempts=$2 AND payload_json=$3)")
        .bind(&job.id).bind(job.attempts).bind(&job.payload).fetch_one(&mut **tx).await?;
    ensure!(active, "authorization job is no longer running");
    let row = sqlx::query("SELECT * FROM authorization_changes WHERE job_id=$1 FOR UPDATE")
        .bind(change)
        .fetch_one(&mut **tx)
        .await?;
    let target: String = row.get("target_id");
    let ty: String = row.get("target_type");
    crate::execution::require_job_resource_scope(&job.id, &format!("authorization:{ty}:{target}"))?;
    let action: String = row.get("action");
    let input: Value = row.get("input");
    let result = json!({"resource_id":target,"additions_count":row.get::<i64,_>("additions"),"removals_count":row.get::<i64,_>("removals")});
    if row.get::<bool, _>("complete") {
        return Ok(Some(result));
    }
    let phase: String = row.get("phase");
    if phase == "sources" {
        // Only the source delta is touched. No complete member/node arrays are read.
        if action == "delete" {
            sqlx::query("UPDATE authorization_groups SET deleting=true WHERE id=$1")
                .bind(&target)
                .execute(&mut **tx)
                .await?;
            set_phase(tx, change, "pairs").await?;
            return Ok(None);
        }
        let step: i32 = row.get("source_step");
        let source_cursor: String = row.get("source_cursor");
        let keys = if ty == "user" {
            vec!["remove_group_ids", "group_ids"]
        } else {
            vec![
                "remove_user_ids",
                "add_user_ids",
                "remove_node_ids",
                "add_node_ids",
            ]
        };
        if let Some(key) = keys.get(step as usize) {
            let batch:Vec<String>=sqlx::query_scalar("SELECT item_id FROM authorization_change_items WHERE job_id=$1 AND kind=$2 AND item_id>$3 ORDER BY item_id LIMIT $4")
                .bind(change).bind(key).bind(&source_cursor).bind(PAGE).fetch_all(&mut **tx).await?;
            if !batch.is_empty() {
                source_page(tx, &ty, &target, key, &batch).await?;
                sqlx::query("UPDATE authorization_changes SET source_cursor=$2 WHERE job_id=$1")
                    .bind(change)
                    .bind(batch.last().unwrap())
                    .execute(&mut **tx)
                    .await?;
            } else {
                sqlx::query("UPDATE authorization_changes SET source_step=source_step+1,source_cursor='' WHERE job_id=$1").bind(change).execute(&mut **tx).await?;
            }
        } else {
            if ty == "group"
                && let Some(name) = input["name"].as_str()
            {
                sqlx::query("UPDATE authorization_groups SET name=$2,updated_at=now() WHERE id=$1")
                    .bind(&target)
                    .bind(name)
                    .execute(&mut **tx)
                    .await?;
            }
            set_phase(tx, change, "pairs").await?;
        }
        return Ok(None);
    }
    if phase == "pairs" {
        let current: Option<String> = row.get("page_user");
        let user = if let Some(user) = current {
            user
        } else {
            let after: String = row.get("cursor_user");
            let next = next_user(tx, change, &ty, &target, &action, &after).await?;
            let Some(user) = next else {
                set_phase(tx, change, "cleanup").await?;
                return Ok(None);
            };
            sqlx::query(
                "UPDATE authorization_changes SET page_user=$2,cursor_node='' WHERE job_id=$1",
            )
            .bind(change)
            .bind(&user)
            .execute(&mut **tx)
            .await?;
            user
        };
        let cursor: String = row.get("cursor_node");
        let nodes = candidate_nodes(tx, change, &ty, &target, &action, &user, &cursor).await?;
        if nodes.is_empty() {
            if ty == "group" {
                sqlx::query("UPDATE users SET revision=revision+1,updated_at=now() WHERE id=$1")
                    .bind(&user)
                    .execute(&mut **tx)
                    .await?;
            }
            sqlx::query("UPDATE authorization_changes SET cursor_user=$2,page_user=NULL,cursor_node='' WHERE job_id=$1").bind(change).bind(&user).execute(&mut **tx).await?;
            return Ok(None);
        }
        let (added, removed) = reconcile_page(tx, &user, &nodes).await?;
        sqlx::query("UPDATE authorization_changes SET cursor_node=$2,additions=additions+$3,removals=removals+$4 WHERE job_id=$1")
            .bind(change).bind(nodes.last().unwrap()).bind(added).bind(removed).execute(&mut **tx).await?;
        sqlx::query("UPDATE jobs SET stage='authorization_applying',result_json=jsonb_build_object('stage','authorization_applying','data',jsonb_build_object('additions_count',$2::bigint,'removals_count',$3::bigint)),updated_at=now() WHERE id=$1")
            .bind(&job.id).bind(row.get::<i64,_>("additions")+added).bind(row.get::<i64,_>("removals")+removed).execute(&mut **tx).await?;
        return Ok(None);
    }
    if action == "delete" {
        for (table, column) in [
            ("authorization_group_users", "user_id"),
            ("authorization_group_nodes", "node_id"),
        ] {
            let query = format!(
                "DELETE FROM {table} WHERE group_id=$1 AND {column} IN (SELECT {column} FROM {table} WHERE group_id=$1 ORDER BY {column} LIMIT {PAGE})"
            );
            if sqlx::query(sqlx::AssertSqlSafe(query))
                .bind(&target)
                .execute(&mut **tx)
                .await?
                .rows_affected()
                > 0
            {
                return Ok(None);
            }
        }
    }
    if sqlx::query("DELETE FROM authorization_exclusions WHERE ctid IN (SELECT ctid FROM authorization_exclusions WHERE job_id=$1 LIMIT $2)")
        .bind(change).bind(PAGE).execute(&mut **tx).await?.rows_affected()>0 {return Ok(None);}
    if action == "delete" {
        sqlx::query("DELETE FROM authorization_groups WHERE id=$1")
            .bind(&target)
            .execute(&mut **tx)
            .await?;
    }
    sqlx::query("UPDATE authorization_changes SET complete=true,phase='complete' WHERE job_id=$1")
        .bind(change)
        .execute(&mut **tx)
        .await?;
    super::super::audit_on(
        tx,
        &row.get::<String, _>("actor_id"),
        "authorization_change.applied",
        if ty == "group" {
            "authorization_group"
        } else {
            "user"
        },
        &target,
        result.clone(),
    )
    .await?;
    Ok(Some(result))
}
async fn set_phase(tx: &mut Transaction<'_, Postgres>, change: &str, phase: &str) -> Result<()> {
    sqlx::query("UPDATE authorization_changes SET phase=$2 WHERE job_id=$1")
        .bind(change)
        .bind(phase)
        .execute(&mut **tx)
        .await?;
    Ok(())
}
async fn source_page(
    tx: &mut Transaction<'_, Postgres>,
    ty: &str,
    target: &str,
    key: &str,
    batch: &[String],
) -> Result<()> {
    if ty == "user" {
        // Lock affected groups before the user, matching the rest of the graph writers.
        sqlx::query("SELECT id FROM authorization_groups WHERE id=ANY($1) ORDER BY id FOR UPDATE")
            .bind(batch)
            .fetch_all(&mut **tx)
            .await?;
        sqlx::query("SELECT id FROM users WHERE id=$1 FOR NO KEY UPDATE")
            .bind(target)
            .fetch_optional(&mut **tx)
            .await?;
        if key == "remove_group_ids" {
            sqlx::query(
                "DELETE FROM authorization_group_users WHERE user_id=$1 AND group_id=ANY($2)",
            )
            .bind(target)
            .bind(batch)
            .execute(&mut **tx)
            .await?;
        } else {
            sqlx::query("INSERT INTO authorization_group_users(group_id,user_id,created_at) SELECT g.id,$1,now() FROM authorization_groups g WHERE g.id=ANY($2) AND NOT g.deleting ORDER BY g.id ON CONFLICT DO NOTHING").bind(target).bind(batch).execute(&mut **tx).await?;
        }
        sqlx::query(
            "UPDATE authorization_groups SET revision=revision+1,updated_at=now() WHERE id=ANY($1)",
        )
        .bind(batch)
        .execute(&mut **tx)
        .await?;
    } else {
        sqlx::query("SELECT id FROM authorization_groups WHERE id=$1 FOR UPDATE")
            .bind(target)
            .fetch_optional(&mut **tx)
            .await?;
        let (table, column, entity) = if key.ends_with("user_ids") {
            ("authorization_group_users", "user_id", "users")
        } else {
            ("authorization_group_nodes", "node_id", "nodes")
        };
        let query = if key.starts_with("remove") {
            format!("DELETE FROM {table} WHERE group_id=$1 AND {column}=ANY($2)")
        } else {
            format!(
                "INSERT INTO {table}(group_id,{column},created_at) SELECT $1,id,now() FROM {entity} WHERE id=ANY($2) ORDER BY id ON CONFLICT DO NOTHING"
            )
        };
        sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(target)
            .bind(batch)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}
async fn next_user(
    tx: &mut Transaction<'_, Postgres>,
    change: &str,
    ty: &str,
    target: &str,
    action: &str,
    after: &str,
) -> Result<Option<String>> {
    if ty == "user" {
        return Ok(
            sqlx::query_scalar("SELECT id FROM users WHERE id=$1 AND id>$2")
                .bind(target)
                .bind(after)
                .fetch_optional(&mut **tx)
                .await?,
        );
    }
    Ok(sqlx::query_scalar(r#"
SELECT id FROM (
 (SELECT gu.user_id AS id FROM authorization_group_users gu WHERE gu.group_id=$1 AND gu.user_id>$2
 AND ($3 OR EXISTS(SELECT 1 FROM authorization_change_items WHERE job_id=$4 AND kind IN ('add_node_ids','remove_node_ids')))
 ORDER BY gu.user_id LIMIT 1)
 UNION
 (SELECT x.item_id AS id FROM authorization_change_items x JOIN users u ON u.id=x.item_id
 WHERE x.job_id=$4 AND x.kind IN ('add_user_ids','remove_user_ids') AND x.item_id>$2 ORDER BY x.item_id LIMIT 1)
) candidates ORDER BY id LIMIT 1
"#).bind(target).bind(after).bind(action=="delete").bind(change).fetch_optional(&mut **tx).await?)
}
async fn candidate_nodes(
    tx: &mut Transaction<'_, Postgres>,
    change: &str,
    ty: &str,
    target: &str,
    action: &str,
    user: &str,
    after: &str,
) -> Result<Vec<String>> {
    if ty == "user" {
        return Ok(sqlx::query_scalar(r#"
SELECT id FROM (
 (SELECT node_id AS id FROM node_assignments WHERE user_id=$1 AND node_id>$2 ORDER BY node_id LIMIT $3)
 UNION
 (SELECT gn.node_id AS id FROM authorization_group_users gu JOIN authorization_group_nodes gn USING(group_id)
 WHERE gu.user_id=$1 AND gn.node_id>$2 ORDER BY gn.node_id LIMIT $3)
) candidates ORDER BY id LIMIT $3
"#).bind(user).bind(after).bind(PAGE).fetch_all(&mut **tx).await?);
    }
    Ok(sqlx::query_scalar(r#"
SELECT c.id FROM (
 (SELECT x.item_id AS id FROM authorization_change_items x WHERE x.job_id=$1 AND x.kind IN ('add_node_ids','remove_node_ids') AND x.item_id>$2 ORDER BY x.item_id LIMIT $3)
 UNION
 (SELECT gn.node_id AS id FROM authorization_group_nodes gn WHERE gn.group_id=$4 AND gn.node_id>$2
 AND ($5 OR EXISTS(SELECT 1 FROM authorization_change_items WHERE job_id=$1 AND kind IN ('add_user_ids','remove_user_ids') AND item_id=$6))
 ORDER BY gn.node_id LIMIT $3)
) c JOIN nodes n ON n.id=c.id ORDER BY c.id LIMIT $3
"#).bind(change).bind(after).bind(PAGE).bind(target).bind(action=="delete").bind(user).fetch_all(&mut **tx).await?)
}
async fn reconcile_page(
    tx: &mut Transaction<'_, Postgres>,
    user: &str,
    nodes: &[String],
) -> Result<(i64, i64)> {
    // Node key locks precede user locks; only this page's resources are locked.
    sqlx::query("SELECT id FROM nodes WHERE id=ANY($1) ORDER BY id FOR KEY SHARE")
        .bind(nodes)
        .fetch_all(&mut **tx)
        .await?;
    // Publish an MVCC fence even when the row values do not change. A waiter
    // with an older SERIALIZABLE snapshot must retry before reconciling pairs.
    let exists: Option<String> =
        sqlx::query_scalar("UPDATE users SET updated_at=updated_at WHERE id=$1 RETURNING id")
            .bind(user)
            .fetch_optional(&mut **tx)
            .await?;
    if exists.is_none() {
        return Ok((0, 0));
    }
    let desired:Vec<String>=sqlx::query_scalar("SELECT n FROM unnest($2::text[])n WHERE EXISTS(SELECT 1 FROM authorization_group_users gu JOIN authorization_group_nodes gn USING(group_id) JOIN authorization_groups g ON g.id=gu.group_id WHERE gu.user_id=$1 AND gn.node_id=n AND NOT g.deleting) ORDER BY n")
        .bind(user).bind(nodes).fetch_all(&mut **tx).await?;
    let added:Vec<Pair>=sqlx::query_as("INSERT INTO node_assignments(user_id,node_id,created_at) SELECT $1,n,now() FROM unnest($2::text[])n ORDER BY n ON CONFLICT DO NOTHING RETURNING user_id,node_id")
        .bind(user).bind(&desired).fetch_all(&mut **tx).await?;
    let removed:Vec<Pair>=sqlx::query_as("DELETE FROM node_assignments WHERE user_id=$1 AND node_id=ANY($2) AND NOT(node_id=ANY($3)) RETURNING user_id,node_id")
        .bind(user).bind(nodes).bind(&desired).fetch_all(&mut **tx).await?;
    crate::credentials::authorization::lock_links_on(tx, user, nodes).await?;
    let plan = crate::credentials::authorization::plan_on(tx, &added).await?;
    crate::credentials::authorization::commit_prepared(
        tx,
        crate::credentials::authorization::prepare_plan(plan),
        &added,
    )
    .await?;
    crate::kick_requests::clear_reason_for_pairs(tx, &added, "authorization_group_removed").await?;
    crate::kick_requests::enqueue_pairs(tx, &removed, "authorization_group_removed").await?;
    let changed: Vec<_> = added.iter().chain(&removed).cloned().collect();
    crate::credentials::authorization::reconcile_on(tx, &[user.to_owned()], &changed, None).await?;
    Ok((added.len() as i64, removed.len() as i64))
}

#[cfg(test)]
pub(crate) async fn execute_change_for_test(state: &AppState, id: &str) -> String {
    let mut tx = state.pool.begin().await.unwrap();
    let row=sqlx::query("UPDATE jobs SET status='running',attempts=attempts+1,stage='starting' WHERE id=$1 RETURNING *").bind(id).fetch_one(&mut *tx).await.unwrap();
    let job = JobInput {
        id: id.into(),
        kind: row.get("kind"),
        node_id: None,
        target_revision: None,
        payload: row.get("payload_json"),
        attempts: row.get("attempts"),
    };
    let mut resources = vec![format!("job:{id}"), row.get::<String, _>("resource_key")];
    resources.sort();
    let mut leases = Vec::new();
    for resource in resources {
        leases.push(
            crate::execution::acquire(&mut tx, &resource, "fixture", id)
                .await
                .unwrap()
                .unwrap(),
        );
    }
    tx.commit().await.unwrap();
    Box::pin(crate::jobs::execute_for_test(state.clone(), job, leases)).await;
    let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id=$1")
        .bind(id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    status
}
#[cfg(test)]
pub(crate) async fn finish_for_test(state: &AppState, id: &str) {
    let status = Box::pin(execute_change_for_test(state, id)).await;
    assert_eq!(
        status,
        "succeeded",
        "{:?}",
        sqlx::query_scalar::<_, Option<String>>("SELECT error_message FROM jobs WHERE id=$1")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .unwrap()
    );
}

#[cfg(test)]
mod tests;
