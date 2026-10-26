use super::*;

#[tokio::test]
async fn node_cascade_skips_rollup_updates_and_preserves_other_nodes() {
    let pool = super::tests::seed().await;
    sqlx::raw_sql(
        "INSERT INTO traffic_records(id,node_id,user_id,instance_id,baseline_tx,baseline_rx,delta_tx,delta_rx,sampled_at)
         SELECT n||i,n,'u','instance',0,0,10,20,'2026-01-01'::timestamptz+i*interval '1 second'
         FROM unnest(ARRAY['n1','n2']) n CROSS JOIN generate_series(1,2000) i;
         INSERT INTO node_network_samples(node_id,period_id,delta_tx,delta_rx,sampled_at)
         SELECT n,'period',10,20,'2026-01-01'::timestamptz+i*interval '1 second'
         FROM unnest(ARRAY['n1','n2']) n CROSS JOIN generate_series(1,2000) i;
         INSERT INTO proxy_probe_samples(node_id,revision,status,sampled_at)
         SELECT n,1,'ok','2026-01-01'::timestamptz+i*interval '1 second'
         FROM unnest(ARRAY['n1','n2']) n CROSS JOIN generate_series(1,2000) i;",
    )
    .execute(&pool)
    .await
    .unwrap();
    refresh(&pool).await.unwrap();
    // Fail deterministically if a cascade adjusts rollups one sample at a time.
    sqlx::raw_sql(
        "CREATE FUNCTION reject_rollup_update() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN RAISE EXCEPTION 'node cascade must not update rollups'; END $$;
         CREATE TRIGGER reject_update BEFORE UPDATE ON traffic_minute_totals FOR EACH ROW EXECUTE FUNCTION reject_rollup_update();
         CREATE TRIGGER reject_update BEFORE UPDATE ON user_node_usage FOR EACH ROW EXECUTE FUNCTION reject_rollup_update();
         CREATE TRIGGER reject_update BEFORE UPDATE ON probe_minute_totals FOR EACH ROW EXECUTE FUNCTION reject_rollup_update();",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("DELETE FROM nodes WHERE id='n1'")
        .execute(&pool)
        .await
        .unwrap();
    refresh(&pool).await.unwrap();
    for table in [
        "traffic_records",
        "node_network_samples",
        "proxy_probe_samples",
        "traffic_minute_totals",
        "traffic_hour_totals",
        "user_node_usage",
        "probe_minute_totals",
    ] {
        let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT count(*) FROM {table} WHERE node_id='n1'"
        )))
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 0, "{table} must cascade with the deleted node");
    }
    let surviving: (i64, i64, i64) = sqlx::query_as(
        "SELECT tx::bigint,rx::bigint,samples FROM user_node_usage WHERE node_id='n2'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(surviving, (20000, 40000, 2000));
    for table in ["traffic_minute_totals", "traffic_hour_totals"] {
        let totals: Vec<(String, i64, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT source,sum(tx)::bigint,sum(rx)::bigint FROM {table} WHERE node_id='n2' GROUP BY source ORDER BY source"
        )))
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            totals,
            vec![
                ("network".into(), 20000, 40000),
                ("users".into(), 20000, 40000)
            ]
        );
    }
    let probes: (i64, i64) = sqlx::query_as(
        "SELECT sum(attempts)::bigint,sum(successes)::bigint FROM probe_minute_totals WHERE node_id='n2'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(probes, (2000, 2000));
}
