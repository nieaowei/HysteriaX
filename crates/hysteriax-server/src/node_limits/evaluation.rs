//! One evaluator for inline changes and bounded due batches. SQL round trips do
//! not grow with the number of nodes evaluated in a batch.
use super::*;

#[derive(Serialize)]
struct Evaluation {
    node_id: String,
    usage_bytes: i64,
    period_id: String,
    next_reset_at: Option<DateTime<Utc>>,
    next_evaluation_at: Option<DateTime<Utc>>,
    restricted: bool,
    changed: bool,
    audit_id: String,
    alerts: Vec<Value>,
    reasons: Vec<&'static str>,
}

fn calculate(row: &sqlx::postgres::PgRow, now: DateTime<Utc>) -> Result<Evaluation> {
    let package: Package = serde_json::from_value(row.get("config"))?;
    let mut usage: i64 = row.get("usage_bytes");
    let mut period: String = row.get("period_id");
    let mut reset: Option<DateTime<Utc>> = row.get("next_reset_at");
    if package.cycle == "monthly" && reset.is_none_or(|at| at <= now) {
        if reset.is_some() {
            usage = 0;
        }
        period = Uuid::new_v4().to_string();
        reset = next_reset(&package, now);
    }
    let restrictions = reasons(&package, usage, now);
    let restricted = !restrictions.is_empty();
    let mut alerts = Vec::new();
    if let Some(expiry) = package.expires_at {
        let kind = if expiry <= now {
            Some("expired")
        } else if expiry - now <= chrono::Duration::days(package.expiry_warning_days) {
            Some("expiring")
        } else {
            None
        };
        if let Some(kind) = kind {
            alerts.push(json!({"id":Uuid::new_v4(),"kind":kind,"scope":expiry.to_rfc3339()}));
        }
    }
    if let Some(quota) = package.quota_bytes {
        let kind = if usage >= quota {
            Some("quota_exhausted")
        } else if i128::from(usage) * 100
            >= i128::from(quota) * i128::from(package.traffic_warning_percent)
        {
            Some("traffic_warning")
        } else {
            None
        };
        if let Some(kind) = kind {
            alerts.push(json!({"id":Uuid::new_v4(),"kind":kind,"scope":period}));
        }
    }
    let warning = package
        .expires_at
        .map(|at| at - chrono::Duration::days(package.expiry_warning_days));
    let next_evaluation_at = [reset, warning, package.expires_at]
        .into_iter()
        .flatten()
        .filter(|at| *at > now)
        .min();
    Ok(Evaluation {
        node_id: row.get("node_id"),
        usage_bytes: usage,
        period_id: period,
        next_reset_at: reset,
        next_evaluation_at,
        restricted,
        changed: restricted != row.get::<bool, _>("restricted"),
        audit_id: Uuid::new_v4().to_string(),
        alerts,
        reasons: restrictions,
    })
}

pub(super) async fn evaluate_many(
    tx: &mut Transaction<'_, Postgres>,
    ids: &[String],
    now: DateTime<Utc>,
) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    // All callers own parent node locks first. Sort child rows independently of
    // due order, matching saves, samples, deletion and restriction dispatch.
    let rows = sqlx::query(
        "SELECT * FROM node_packages WHERE node_id=ANY($1::text[]) ORDER BY node_id FOR UPDATE",
    )
    .bind(ids)
    .fetch_all(&mut **tx)
    .await?;
    let evaluations: Vec<Evaluation> = rows
        .iter()
        .map(|row| calculate(row, now))
        .collect::<Result<_>>()?;
    let payload = serde_json::to_value(evaluations)?;
    sqlx::query(include_str!("evaluation.sql"))
        .bind(payload)
        .bind(now)
        .bind(maintenance::KIND)
        .execute(&mut **tx)
        .await?;
    Ok(())
}
