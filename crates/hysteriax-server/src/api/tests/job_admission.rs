use super::*;

#[tokio::test]
async fn immediate_job_admission_uses_database_time_despite_application_clock_skew() {
    for offset in [-120, 120] {
        let pool = db::test_pool().await;
        TEST_CLOCK_OFFSET_SECONDS
            .scope(offset, async {
                let before: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
                assert!((now() - before).num_seconds().abs() >= 119);
                let mut tx = pool.begin().await.unwrap();
                let id = enqueue_job_in_tx(&mut tx, "ssh-test", None, None)
                    .await
                    .unwrap();
                tx.commit().await.unwrap();
                let row = sqlx::query("SELECT j.available_at,j.created_at,j.updated_at,e.created_at AS event_at,clock_timestamp() AS observed FROM jobs j JOIN job_events e ON e.job_id=j.id AND e.event_type='job.queued' WHERE j.id=$1")
                    .bind(&id).fetch_one(&pool).await.unwrap();
                let available: DateTime<Utc> = row.get("available_at");
                let observed: DateTime<Utc> = row.get("observed");
                assert!(before <= available && available <= observed);
                for column in ["created_at", "updated_at", "event_at"] {
                    assert_eq!(row.get::<DateTime<Utc>, _>(column), available);
                }
                assert_eq!(
                    crate::jobs::claim_next(&pool).await.unwrap().unwrap().id,
                    id
                );

                // Explicit backoff is still authoritative; a test helper must
                // not force a future job into eligibility to hide clock skew.
                let mut tx = pool.begin().await.unwrap();
                let later = enqueue_job_in_tx(&mut tx, "ssh-test", None, None)
                    .await
                    .unwrap();
                sqlx::query("UPDATE jobs SET available_at=clock_timestamp()+interval '30 seconds' WHERE id=$1")
                    .bind(&later).execute(&mut *tx).await.unwrap();
                tx.commit().await.unwrap();
                assert!(crate::jobs::claim_next(&pool).await.unwrap().is_none());
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT attempts FROM jobs WHERE id=$1")
                        .bind(later)
                        .fetch_one(&pool)
                        .await
                        .unwrap(),
                    0
                );
            })
            .await;
    }
}
