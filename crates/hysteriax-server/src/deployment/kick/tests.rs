use super::*;
use std::sync::Mutex;

struct FakeNode {
    online: Mutex<serde_json::Map<String, Value>>,
    calls: Mutex<Vec<Vec<String>>>,
    fail_call: Option<usize>,
}
impl Transport for FakeNode {
    async fn online(&self) -> Result<Value> {
        Ok(Value::Object(self.online.lock().unwrap().clone()))
    }
    async fn disconnect(&self, users: &[String]) -> Result<()> {
        let index = {
            let mut calls = self.calls.lock().unwrap();
            calls.push(users.to_vec());
            calls.len()
        };
        if self.fail_call == Some(index) {
            anyhow::bail!("injected transport failure");
        }
        for user in users {
            self.online.lock().unwrap().remove(user);
        }
        Ok(())
    }
}
async fn fixture(size: usize) -> (crate::state::AppState, JobInput, FakeNode) {
    let state = kick_requests::tests::fixture().await;
    let users: Vec<_> = (0..size).map(|i| format!("user-{i:05}")).collect();
    let mut tx = state.pool.begin().await.unwrap();
    let id = kick_requests::enqueue_many(&mut tx, "node", &users, "authorization_group_removed")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    sqlx::query("UPDATE jobs SET status='running' WHERE id=$1")
        .bind(&id)
        .execute(&state.pool)
        .await
        .unwrap();
    let job = kick_requests::tests::job(&state, &id).await;
    let node = FakeNode {
        online: Mutex::new(users.into_iter().map(|u| (u, json!(1))).collect()),
        calls: Mutex::new(vec![]),
        fail_call: None,
    };
    (state, job, node)
}

#[tokio::test]
async fn ten_thousand_users_have_one_job_and_fifty_bounded_remote_batches() {
    let (state, job, node) = fixture(10_000).await;
    let mut batches = 0;
    loop {
        let rows = kick_requests::batch(&state.pool, &job, None).await.unwrap();
        if rows.is_empty() {
            break;
        }
        assert!(rows.len() <= 200);
        disconnect_batch(&state.pool, &job, rows, &node)
            .await
            .unwrap();
        batches += 1;
    }
    assert_eq!(batches, 50);
    let calls = node.calls.lock().unwrap();
    assert_eq!(calls.len(), 50);
    assert!(calls.iter().all(|c| c.len() == 200));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind='kick'")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    let done: i64 =
        sqlx::query_scalar("SELECT count(*) FROM kick_requests WHERE state='completed'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(done, 10_000);
    assert_eq!(job.payload, json!({"scope":"node"}));
}

#[tokio::test]
async fn failed_batch_resumes_without_repeating_completed_users() {
    let (state, job, mut node) = fixture(450).await;
    node.fail_call = Some(2);
    let first = kick_requests::batch(&state.pool, &job, None).await.unwrap();
    disconnect_batch(&state.pool, &job, first, &node)
        .await
        .unwrap();
    let second = kick_requests::batch(&state.pool, &job, None).await.unwrap();
    assert!(
        disconnect_batch(&state.pool, &job, second, &node)
            .await
            .is_err()
    );
    let done: i64 =
        sqlx::query_scalar("SELECT count(*) FROM kick_requests WHERE state='completed'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(done, 200);
    node.fail_call = None;
    let remaining = kick_requests::batch(&state.pool, &job, None).await.unwrap();
    assert_eq!(remaining[0].user, "user-00200");
    disconnect_batch(&state.pool, &job, remaining, &node)
        .await
        .unwrap();
    let last = kick_requests::batch(&state.pool, &job, None).await.unwrap();
    assert_eq!(last.len(), 50);
    disconnect_batch(&state.pool, &job, last, &node)
        .await
        .unwrap();
    assert!(
        kick_requests::batch(&state.pool, &job, None)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn old_checkpoint_cannot_clear_new_generation_and_regrant_is_not_dispatched() {
    let (state, job, node) = fixture(2).await;
    let old = kick_requests::batch(&state.pool, &job, None).await.unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    kick_requests::enqueue_many(
        &mut tx,
        "node",
        &[old[0].user.clone()],
        "credentials_revoked",
    )
    .await
    .unwrap();
    kick_requests::clear_reason(
        &mut tx,
        Some("node"),
        Some(&old[1].user),
        "authorization_group_removed",
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    kick_requests::acknowledge(&state.pool, &job, &old, "completed")
        .await
        .unwrap();
    assert_eq!(
        kick_requests::batch(&state.pool, &job, None)
            .await
            .unwrap()
            .len(),
        1
    );
    disconnect_batch(&state.pool, &job, old, &node)
        .await
        .unwrap();
    assert_eq!(
        *node.calls.lock().unwrap(),
        vec![vec!["user-00000".to_owned()]]
    );
}
