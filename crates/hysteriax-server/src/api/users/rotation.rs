use super::{enqueue_job_with_payload_in_tx, lock_user_graph_on, now};
use crate::{api::generate_token, error::ApiError, security::token_digest, state::AppState};
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction};

#[derive(Clone)]
pub(super) struct Credential {
    node: String,
    plaintext: String,
    hash: String,
    ciphertext: String,
}

/// Freeze only the assignment IDs here. Revision and membership validation must
/// happen after receipt arbitration, so a concurrent committed replay wins first.
pub(super) async fn prepare(state: &AppState, user: &str) -> Result<Vec<Credential>, ApiError> {
    let nodes: Vec<String> = sqlx::query_scalar(
        "SELECT node_id FROM node_assignments WHERE user_id=$1 ORDER BY node_id",
    )
    .bind(user)
    .fetch_all(&state.pool)
    .await?;
    nodes
        .into_iter()
        .map(|node| {
            let plaintext = generate_token();
            Ok(Credential {
                node,
                hash: token_digest(&plaintext),
                ciphertext: state.secrets.encrypt(&plaintext)?,
                plaintext,
            })
        })
        .collect()
}

pub(super) async fn commit_on(
    tx: &mut Transaction<'static, Postgres>,
    user_id: String,
    expected: i64,
    prepared: Vec<Credential>,
    actor: String,
) -> Result<Value, ApiError> {
    if expected < 1 {
        return Err(ApiError::bad_request("expected_revision must be positive"));
    }
    let user = lock_user_graph_on(tx, &user_id, false).await?;
    let revision: i64 = user.get("revision");
    if revision != expected {
        return Err(ApiError::conflict(format!(
            "user revision is {revision}; reload before editing"
        )));
    }
    let nodes: Vec<String> = sqlx::query_scalar(
        "SELECT node_id FROM node_assignments WHERE user_id=$1 ORDER BY node_id FOR UPDATE",
    )
    .bind(&user_id)
    .fetch_all(&mut **tx)
    .await?;
    if nodes
        .iter()
        .map(String::as_str)
        .ne(prepared.iter().map(|credential| credential.node.as_str()))
    {
        return Err(ApiError::conflict(
            "user assignments changed while preparing credential rotation; reload before retrying",
        ));
    }
    let timestamp = now();
    let mut credentials = Vec::with_capacity(prepared.len());
    for credential in prepared {
        sqlx::query("UPDATE node_assignments SET credential_hash=$1,credential_enc=$2 WHERE user_id=$3 AND node_id=$4")
            .bind(credential.hash).bind(credential.ciphertext).bind(&user_id).bind(&credential.node)
            .execute(&mut **tx).await?;
        enqueue_job_with_payload_in_tx(
            tx,
            "kick",
            Some(&credential.node),
            None,
            json!({"user_id":user_id}),
        )
        .await?;
        credentials.push(json!({"node_id":credential.node,"credential":credential.plaintext}));
    }
    let next_revision = revision + 1;
    let changed =
        sqlx::query("UPDATE users SET revision=$1,updated_at=$2 WHERE id=$3 AND revision=$4")
            .bind(next_revision)
            .bind(timestamp)
            .bind(&user_id)
            .bind(revision)
            .execute(&mut **tx)
            .await?;
    if changed.rows_affected() != 1 {
        return Err(ApiError::conflict(
            "user changed while rotating credentials",
        ));
    }
    super::super::audit_on(
        tx,
        &actor,
        "user.credentials_rotated",
        "user",
        &user_id,
        json!({"revision":next_revision,"nodes":credentials.len()}),
    )
    .await?;
    Ok(
        json!({"user_id":user_id,"revision":next_revision,"credentials":credentials,"note":"Save these credentials now; this response is the only plaintext copy."}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn fixture() -> AppState {
        let state = crate::kick_requests::tests::fixture().await;
        sqlx::query("INSERT INTO node_assignments(user_id,node_id,credential_hash,credential_enc,created_at) VALUES('user','node','old',$1,clock_timestamp())")
            .bind(state.secrets.encrypt("old").unwrap()).execute(&state.pool).await.unwrap();
        state
    }

    #[tokio::test]
    async fn changed_assignment_set_rejects_prepared_passwords_without_changing_revision_or_enqueuing()
     {
        let state = fixture().await;
        let prepared = prepare(&state, "user").await.unwrap();
        sqlx::query("DELETE FROM node_assignments")
            .execute(&state.pool)
            .await
            .unwrap();
        let error = crate::db::serializable(&state.pool, move |tx| {
            let prepared = prepared.clone();
            Box::pin(async move { commit_on(tx, "user".into(), 1, prepared, "admin".into()).await })
        })
        .await
        .unwrap_err();
        assert_eq!(error.status, axum::http::StatusCode::CONFLICT);
        let counts: (i64,i64,i64) = sqlx::query_as("SELECT (SELECT revision FROM users WHERE id='user'),(SELECT count(*) FROM jobs),(SELECT count(*) FROM audit_records)")
            .fetch_one(&state.pool).await.unwrap();
        assert_eq!(counts, (1, 0, 0));
    }

    #[tokio::test]
    async fn native_retry_reuses_prepared_password_hash_and_ciphertext() {
        let state = fixture().await;
        let prepared = prepare(&state, "user").await.unwrap();
        let expected = (prepared[0].hash.clone(), prepared[0].ciphertext.clone());
        let plaintext = prepared[0].plaintext.clone();
        sqlx::raw_sql("CREATE SEQUENCE password_attempt; CREATE FUNCTION retry_password() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF nextval('password_attempt')=1 THEN RAISE EXCEPTION 'retry' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_password BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION retry_password()")
            .execute(&state.pool).await.unwrap();
        let response = crate::db::serializable(&state.pool, move |tx| {
            let prepared = prepared.clone();
            Box::pin(async move { commit_on(tx, "user".into(), 1, prepared, "admin".into()).await })
        })
        .await
        .unwrap();
        assert_eq!(response["credentials"][0]["credential"], plaintext);
        assert_eq!(
            sqlx::query_as::<_, (String, String)>(
                "SELECT credential_hash,credential_enc FROM node_assignments"
            )
            .fetch_one(&state.pool)
            .await
            .unwrap(),
            expected
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT last_value FROM password_attempt")
                .fetch_one(&state.pool)
                .await
                .unwrap(),
            2
        );
    }
}
