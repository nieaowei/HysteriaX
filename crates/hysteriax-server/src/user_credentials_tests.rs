use super::*;
use crate::db;
use sqlx::Row;

#[tokio::test]
async fn migration_preserves_existing_passwords_tokens_and_revocation_history() {
    let pool = db::test_pool().await;
    // Rebuild this isolated schema to exercise the actual 8 -> 9 upgrade.
    let mut connection = pool.acquire().await.unwrap();
    let schema: String = sqlx::query_scalar("SELECT current_schema()")
        .fetch_one(&mut *connection)
        .await
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "DROP SCHEMA {schema} CASCADE; CREATE SCHEMA {schema}; SET search_path TO {schema}"
    )))
    .execute(&mut *connection)
    .await
    .unwrap();
    for migration in db::MIGRATOR
        .iter()
        .filter(|migration| migration.version < 9)
    {
        sqlx::raw_sql(sqlx::AssertSqlSafe(migration.sql.as_ref()))
            .execute(&mut *connection)
            .await
            .unwrap();
    }
    let secrets = SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
    let password = secrets.encrypt("existing-password").unwrap();
    let active = secrets.encrypt("existing-token").unwrap();
    let revoked = secrets.encrypt("revoked-token").unwrap();
    sqlx::query("INSERT INTO users(id,name,enabled,connection_credential_hash,connection_credential_enc,created_at,updated_at) VALUES('user','User',TRUE,$1,$2,now(),now())")
        .bind(token_digest("existing-password")).bind(&password).execute(&mut *connection).await.unwrap();
    sqlx::query("INSERT INTO subscription_credentials(id,user_id,token_hash,token_enc,created_at,revoked_at) VALUES('active','user',$1,$2,now(),NULL),('revoked','user',$3,$4,now(),now())")
        .bind(token_digest("existing-token")).bind(&active).bind(token_digest("revoked-token")).bind(&revoked)
        .execute(&mut *connection).await.unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/0009_unify_user_credentials.sql"
    ))
    .execute(&mut *connection)
    .await
    .unwrap();
    let rows = sqlx::query("SELECT kind,secret_hash,secret_enc,revoked_at IS NOT NULL AS revoked FROM user_credentials ORDER BY kind,revoked_at NULLS FIRST")
        .fetch_all(&mut *connection).await.unwrap();
    assert_eq!(rows.len(), 3);
    for (row, (plain, cipher, is_revoked)) in rows.iter().zip([
        ("existing-password", &password, false),
        ("existing-token", &active, false),
        ("revoked-token", &revoked, true),
    ]) {
        assert_eq!(row.get::<String, _>("secret_hash"), token_digest(plain));
        assert_eq!(&row.get::<String, _>("secret_enc"), cipher);
        assert_eq!(secrets.decrypt(cipher).unwrap(), plain);
        assert_eq!(row.get::<bool, _>("revoked"), is_revoked);
    }
    let (old_table, old_columns): (bool, i64) = sqlx::query_as("SELECT to_regclass('subscription_credentials') IS NOT NULL,(SELECT count(*) FROM information_schema.columns WHERE table_schema=current_schema() AND table_name='users' AND column_name IN ('connection_credential_hash','connection_credential_enc'))")
        .fetch_one(&mut *connection).await.unwrap();
    assert!(!old_table);
    assert_eq!(old_columns, 0);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM credential_directory WHERE owner_user_id='user'"
        )
        .fetch_one(&mut *connection)
        .await
        .unwrap(),
        3
    );
    assert!(sqlx::query("INSERT INTO user_credentials(id,user_id,kind,secret_hash,secret_enc,created_at) VALUES('duplicate','user','connection','different','cipher',now())")
        .execute(&mut *connection).await.is_err());
    sqlx::query("DELETE FROM users WHERE id='user'")
        .execute(&mut *connection)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM user_credentials")
            .fetch_one(&mut *connection)
            .await
            .unwrap(),
        0
    );
}
