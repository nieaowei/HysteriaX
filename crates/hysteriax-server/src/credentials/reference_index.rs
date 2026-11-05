//! Configuration references are committed with their encrypted source.
//! Only explicit maintenance imports may leave a dirty source between commits.
use crate::{db, error::ApiError, security::SecretBox};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool, Postgres, Row, Transaction};

pub async fn begin_read(
    pool: &PgPool,
    _secrets: &SecretBox,
) -> Result<crate::read_snapshot::Reader, ApiError> {
    if let Some(reader) = crate::read_snapshot::current().await {
        return Ok(reader);
    }
    Ok(crate::read_snapshot::Reader::owned(
        begin_read_transaction(pool, _secrets).await?,
    ))
}
pub(crate) async fn begin_read_transaction(
    pool: &PgPool,
    _secrets: &SecretBox,
) -> Result<Transaction<'static, Postgres>, ApiError> {
    let mut tx = crate::read_snapshot::transaction(pool).await?;
    assert_clean_on(&mut tx).await?;
    Ok(tx)
}
pub async fn assert_clean_on(connection: &mut PgConnection) -> Result<(), ApiError> {
    let dirty: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM credential_reference_dirty)")
        .fetch_one(connection)
        .await?;
    if dirty {
        return Err(ApiError::new(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "reference_initialization_required",
            "configuration reference initialization is incomplete; run maintenance migration",
        ));
    }
    Ok(())
}
pub async fn assert_ready(pool: &PgPool) -> Result<(), ApiError> {
    let mut connection = pool.acquire().await?;
    assert_clean_on(&mut connection).await?;
    let validated:i64=sqlx::query_scalar("SELECT count(*) FROM pg_constraint WHERE conrelid='credential_reference_edges'::regclass AND conname IN ('reference_edge_shape','reference_edge_version','reference_edge_history') AND convalidated").fetch_one(&mut *connection).await?;
    if validated != 3 {
        return Err(ApiError::new(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "reference_initialization_required",
            "configuration reference constraints are not validated; run maintenance migration",
        ));
    }
    Ok(())
}

/// Flush exactly the changed node and history sources owned by this mutation.
/// The caller owns the node row before changing its configurations.
pub async fn flush_node_on(
    connection: &mut PgConnection,
    secrets: &SecretBox,
    node: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT id FROM nodes WHERE id=$1 FOR NO KEY UPDATE")
        .bind(node)
        .fetch_optional(&mut *connection)
        .await?;
    let sources:Vec<(String,String,i64)>=sqlx::query_as("SELECT d.kind,d.source_id,d.generation FROM credential_reference_dirty d LEFT JOIN config_versions v ON d.kind='history' AND v.id=d.source_id WHERE (d.kind='node' AND d.source_id=$1) OR (d.kind='history' AND v.node_id=$1) ORDER BY d.kind,d.source_id FOR UPDATE OF d")
        .bind(node).fetch_all(&mut *connection).await?;
    for (kind, id, generation) in sources {
        sync_source_on(connection, secrets, &kind, &id, generation).await?;
    }
    Ok(())
}

/// Flush only sources created by one bounded node mutation. Other historical
/// dirty entries remain owned by the independent reference maintenance queue.
pub(crate) async fn flush_node_sources_on(
    connection: &mut PgConnection,
    secrets: &SecretBox,
    node: &str,
    histories: &[String],
) -> Result<(), sqlx::Error> {
    if histories.len() > 8 {
        return Err(sqlx::Error::Protocol(
            "too many owned history sources".into(),
        ));
    }
    sqlx::query("SELECT id FROM nodes WHERE id=$1 FOR NO KEY UPDATE")
        .bind(node)
        .fetch_optional(&mut *connection)
        .await?;
    let sources:Vec<(String,String,i64)>=sqlx::query_as("SELECT d.kind,d.source_id,d.generation FROM credential_reference_dirty d LEFT JOIN config_versions v ON d.kind='history' AND v.id=d.source_id WHERE (d.kind='node' AND d.source_id=$1) OR (d.kind='history' AND d.source_id=ANY($2::text[]) AND v.node_id=$1) ORDER BY d.kind,d.source_id FOR UPDATE OF d")
        .bind(node).bind(histories).fetch_all(&mut *connection).await?;
    for (kind, id, generation) in sources {
        sync_source_on(connection, secrets, &kind, &id, generation).await?;
    }
    Ok(())
}

async fn sync_source_on(
    connection: &mut PgConnection,
    secrets: &SecretBox,
    kind: &str,
    id: &str,
    generation: i64,
) -> Result<(), sqlx::Error> {
    let mut edges = Vec::<Value>::new();
    let mut resources = Vec::<Value>::new();
    let mut parents = std::collections::BTreeSet::<String>::new();
    let source = if kind == "node" {
        sqlx::query("SELECT id AS node_id,NULL::text AS config_version_id,NULL::bigint AS revision,desired_config_enc,deployed_config_enc,ssh_credential_id FROM nodes WHERE id=$1 FOR NO KEY UPDATE")
            .bind(id).fetch_optional(&mut *connection).await?
    } else {
        sqlx::query("SELECT node_id,id AS config_version_id,revision,config_enc AS desired_config_enc,NULL::text AS deployed_config_enc,NULL::text AS ssh_credential_id FROM config_versions WHERE id=$1 FOR UPDATE")
            .bind(id).fetch_optional(&mut *connection).await?
    };
    if let Some(row) = &source {
        if let Some(ssh) = row.get::<Option<String>, _>("ssh_credential_id") {
            parents.insert(ssh);
        }
        for (column, scope) in [
            (
                "desired_config_enc",
                if kind == "node" { "desired" } else { "history" },
            ),
            ("deployed_config_enc", "deployed"),
        ] {
            if let Some(cipher) = row.get::<Option<String>, _>(column) {
                let value: Value =
                    serde_json::from_str(&secrets.decrypt(&cipher).map_err(|_| {
                        sqlx::Error::Protocol(
                            "cannot decrypt configuration reference source".into(),
                        )
                    })?)
                    .map_err(|_| {
                        sqlx::Error::Protocol("invalid configuration reference source".into())
                    })?;
                for resource in crate::api::resources::collect_resource_ids(&value) {
                    resources.push(json!({"source":scope,"resource_id":resource}));
                }
                for (field, reference) in super::references(&value).map_err(|_| {
                    sqlx::Error::Protocol("invalid configuration reference source".into())
                })? {
                    parents.insert(reference.id.clone());
                    edges.push(json!({"source":scope,"field":field,"credential_id":reference.id,"version":reference.version}));
                }
            }
        }
    }
    let parents: Vec<_> = parents.into_iter().collect();
    sqlx::query("SELECT id FROM credentials WHERE id=ANY($1::text[]) ORDER BY id FOR KEY SHARE")
        .bind(&parents)
        .fetch_all(&mut *connection)
        .await?;
    let edges = Value::Array(edges);
    sqlx::query("SELECT v.credential_id,v.version FROM credential_versions v JOIN (SELECT DISTINCT credential_id,version FROM jsonb_to_recordset($1::jsonb) r(credential_id text,version bigint)) r ON r.credential_id=v.credential_id AND r.version=v.version ORDER BY v.credential_id,v.version FOR KEY SHARE OF v")
        .bind(&edges).fetch_all(&mut *connection).await?;
    let resources = Value::Array(resources);
    if let Some(row) = &source {
        sqlx::query("SELECT r.id FROM config_resources r JOIN (SELECT DISTINCT resource_id FROM jsonb_to_recordset($1::jsonb) e(resource_id text)) e ON e.resource_id=r.id WHERE r.node_id=$2 ORDER BY r.id FOR KEY SHARE OF r")
            .bind(&resources).bind(row.get::<String,_>("node_id")).fetch_all(&mut *connection).await?;
    }
    sqlx::query("DELETE FROM config_resource_references WHERE kind=$1 AND source_id=$2")
        .bind(kind)
        .bind(id)
        .execute(&mut *connection)
        .await?;
    sqlx::query("DELETE FROM credential_reference_edges WHERE kind=$1 AND source_id=$2")
        .bind(kind)
        .bind(id)
        .execute(&mut *connection)
        .await?;
    if let Some(row) = source {
        sqlx::query("INSERT INTO config_resource_references(kind,source_id,node_id,config_version_id,config_revision,source,resource_id) SELECT $1,$2,$3,$4,$5,source,resource_id FROM jsonb_to_recordset($6::jsonb) e(source text,resource_id text) ORDER BY resource_id,source")
            .bind(kind).bind(id).bind(row.get::<String,_>("node_id")).bind(row.get::<Option<String>,_>("config_version_id")).bind(row.get::<Option<i64>,_>("revision")).bind(&resources).execute(&mut *connection).await?;
        sqlx::query("INSERT INTO credential_reference_edges(kind,source_id,node_id,config_version_id,config_revision,source,field,credential_id,version) SELECT $1,$2,$3,$4,$5,source,field,credential_id,version FROM jsonb_to_recordset($6::jsonb) r(source text,field text,credential_id text,version bigint) ORDER BY credential_id,version,source,field")
            .bind(kind).bind(id).bind(row.get::<String,_>("node_id")).bind(row.get::<Option<String>,_>("config_version_id")).bind(row.get::<Option<i64>,_>("revision")).bind(&edges).execute(&mut *connection).await?;
    }
    sqlx::query(
        "DELETE FROM credential_reference_dirty WHERE kind=$1 AND source_id=$2 AND generation=$3",
    )
    .bind(kind)
    .bind(id)
    .bind(generation)
    .execute(connection)
    .await?;
    Ok(())
}

/// A bounded maintenance batch. Parent rows precede source and queue rows.
async fn refresh_batch_on(
    connection: &mut PgConnection,
    secrets: &SecretBox,
) -> Result<usize, ApiError> {
    let candidates=sqlx::query("SELECT d.kind,d.source_id,CASE WHEN d.kind='node' THEN n.id ELSE v.node_id END AS node_id FROM credential_reference_dirty d LEFT JOIN nodes n ON d.kind='node' AND n.id=d.source_id LEFT JOIN config_versions v ON d.kind='history' AND v.id=d.source_id ORDER BY d.kind,d.source_id LIMIT 256")
        .fetch_all(&mut *connection).await?;
    let parents: std::collections::BTreeSet<String> = candidates
        .iter()
        .filter_map(|r| r.get::<Option<String>, _>("node_id"))
        .collect();
    let parents: Vec<_> = parents.into_iter().collect();
    let locked: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM nodes WHERE id=ANY($1::text[]) ORDER BY id FOR NO KEY UPDATE SKIP LOCKED",
    )
    .bind(parents)
    .fetch_all(&mut *connection)
    .await?;
    let selected:Vec<Value>=candidates.into_iter().filter(|r|r.get::<Option<String>,_>("node_id").is_none_or(|id|locked.contains(&id)))
        .map(|r|json!({"kind":r.get::<String,_>("kind"),"source_id":r.get::<String,_>("source_id")})).collect();
    let selected = Value::Array(selected);
    let sources:Vec<(String,String,i64)>=sqlx::query_as("SELECT d.kind,d.source_id,d.generation FROM credential_reference_dirty d JOIN jsonb_to_recordset($1::jsonb) c(kind text,source_id text) ON (c.kind,c.source_id)=(d.kind,d.source_id) ORDER BY d.kind,d.source_id FOR UPDATE OF d SKIP LOCKED")
        .bind(selected).fetch_all(&mut *connection).await?;
    let count = sources.len();
    for (kind, id, generation) in sources {
        sync_source_on(connection, secrets, &kind, &id, generation).await?;
    }
    Ok(count)
}
pub async fn initialize(pool: &PgPool, secrets: &SecretBox) -> Result<(), ApiError> {
    let maintenance: bool = sqlx::query_scalar(
        "SELECT coalesce(current_setting('hysteriax.reference_maintenance',true),'off')='on'",
    )
    .fetch_one(pool)
    .await?;
    if !maintenance {
        return Err(ApiError::new(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "reference_maintenance_required",
            "reference initialization requires explicit maintenance",
        ));
    }
    loop {
        let secrets = secrets.clone();
        let count = db::write(pool, move |tx| {
            let secrets = secrets.clone();
            Box::pin(async move { refresh_batch_on(tx, &secrets).await })
        })
        .await?;
        let dirty: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM credential_reference_dirty)")
                .fetch_one(pool)
                .await?;
        if !dirty {
            break;
        }
        if count == 0 {
            return Err(ApiError::new(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "reference_sources_busy",
                "configuration reference sources are busy; retry maintenance",
            ));
        }
    }
    // Initialization is an explicit maintenance operation, never an API read.
    for name in [
        "reference_edge_shape",
        "reference_edge_version",
        "reference_edge_history",
    ] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "ALTER TABLE credential_reference_edges VALIDATE CONSTRAINT {name}"
        )))
        .execute(pool)
        .await?;
    }
    assert_ready(pool).await
}

#[cfg(test)]
#[path = "reference_index_tests.rs"]
mod tests;
