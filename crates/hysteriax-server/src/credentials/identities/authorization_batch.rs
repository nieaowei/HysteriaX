//! Bounded SQL batches for group authorization. Other identity workflows keep
//! their single-identity boundary; all authorization publication remains atomic.
use super::*;
use crate::credentials::authorization::Requirement;
use std::collections::{BTreeMap, BTreeSet};

const PAGE_SIZE: usize = 256;

pub async fn prepare(
    pool: &PgPool,
    secrets: &SecretBox,
    requirements: &[Requirement],
) -> Result<Vec<Vec<PreparedIdentity>>> {
    if requirements.is_empty() {
        return Ok(Vec::new());
    }
    let mut requests = Vec::new();
    let mut generations = BTreeSet::new();
    for (index, r) in requirements.iter().enumerate() {
        let id = Uuid::new_v4().to_string();
        for (offset, version) in r.ca_versions.iter().enumerate() {
            requests.push(json!({"idx":index,"offset":offset,"user_id":r.user_id,"ca_id":r.ca_credential_id,"ca_version":version,"new_id":id}));
            generations.insert((r.ca_credential_id.clone(), *version));
        }
    }
    let mut tx = crate::read_snapshot::transaction(pool).await?;
    let generation_input: Vec<_> = generations
        .into_iter()
        .map(|(ca, version)| json!({"ca_id":ca,"ca_version":version}))
        .collect();
    let payload_rows = sqlx::query("SELECT v.credential_id,v.version,v.payload_enc FROM jsonb_to_recordset($1) AS r(ca_id text,ca_version bigint) JOIN credential_versions v ON v.credential_id=r.ca_id AND v.version=r.ca_version")
        .bind(json!(generation_input)).fetch_all(&mut *tx).await?;
    let payloads: BTreeMap<(String, i64), String> = payload_rows
        .into_iter()
        .map(|r| {
            (
                (r.get("credential_id"), r.get("version")),
                r.get("payload_enc"),
            )
        })
        .collect();
    let mut rows = Vec::new();
    for page in requests.chunks(PAGE_SIZE) {
        let page_rows = sqlx::query(r#"
WITH moment AS MATERIALIZED (SELECT clock_timestamp() AS now)
SELECT r.*,m.now,a.policy_revision,a.client_validity_days,a.renew_before_hours,g.state,c.archived,
 u.revision AS user_revision,u.enabled AND (u.expires_at IS NULL OR u.expires_at>m.now) AND (u.quota_bytes IS NULL OR u.usage_bytes<u.quota_bytes) AS eligible,
 coalesce(i.id,r.new_id) AS identity_id,i.revision AS identity_revision,i.authorization_revision,
 v.credential_version AS existing_version,coalesce(v.next_action_at<=m.now,false) AS needs_ca_update,
 (SELECT max(expires_at) FROM mtls_user_versions WHERE identity_id=i.id AND ca_version=r.ca_version AND NOT discarded) AS previous_expiry
FROM jsonb_to_recordset($1) AS r(idx bigint,"offset" bigint,user_id text,ca_id text,ca_version bigint,new_id text)
CROSS JOIN moment m JOIN users u ON u.id=r.user_id
JOIN mtls_authorities a ON a.credential_id=r.ca_id JOIN credentials c ON c.id=a.credential_id
JOIN mtls_ca_generations g ON g.ca_id=r.ca_id AND g.ca_version=r.ca_version
LEFT JOIN mtls_user_identities i ON i.user_id=r.user_id AND i.ca_id=r.ca_id
LEFT JOIN LATERAL (SELECT credential_version,next_action_at FROM mtls_user_versions
 WHERE identity_id=i.id AND ca_id=r.ca_id AND ca_version=r.ca_version AND NOT discarded
 AND not_before<=m.now AND expires_at>m.now AND (next_action_at>m.now OR expires_at>=g.expires_at-interval '5 minutes')
 ORDER BY credential_version DESC LIMIT 1) v ON true
ORDER BY r.idx,r."offset"
"#).bind(json!(page)).fetch_all(&mut *tx).await?;
        ensure!(
            page_rows.len() == page.len(),
            "user or CA disappeared during preparation; retry"
        );
        rows.extend(page_rows);
    }
    tx.commit().await?;
    let secrets = secrets.clone();
    let count = requirements.len();
    tokio::task::spawn_blocking(move || {
        let mut output = vec![Vec::new(); count];
        let mut decrypted = BTreeMap::<(String, i64), Value>::new();
        for row in rows {
            ensure!(!row.get::<bool, _>("archived"), "CA identity is archived");
            let state: String = row.get("state");
            ensure!(
                state != "retired",
                "retired CA generations cannot sign new identities"
            );
            let ca: String = row.get("ca_id");
            let version: i64 = row.get("ca_version");
            let existing: Option<i64> = row.get("existing_version");
            let payload = if row.get::<bool, _>("eligible") && existing.is_none() {
                let key = (ca.clone(), version);
                if !decrypted.contains_key(&key) {
                    let cipher = payloads.get(&key).context("CA signing material missing")?;
                    decrypted.insert(
                        key.clone(),
                        serde_json::from_str(&secrets.decrypt(cipher)?)?,
                    );
                }
                Some(decrypted[&key].clone())
            } else {
                None
            };
            let candidate = PreparedIdentity {
                id: row.get("identity_id"),
                user_id: row.get("user_id"),
                ca_id: ca,
                ca_version: version,
                policy_revision: row.get("policy_revision"),
                user_revision: row.get("user_revision"),
                expected_identity_revision: row.get("identity_revision"),
                authorization_revision: row.get("authorization_revision"),
                generation_state: state,
                issued: None,
                existing_version: existing,
                needs_ca_update: row.get("needs_ca_update"),
                prepared_material: None,
            };
            let policy = Policy {
                client_validity_days: u16::try_from(row.get::<i32, _>("client_validity_days"))?,
                renew_before_hours: u16::try_from(row.get::<i32, _>("renew_before_hours"))?,
            };
            let candidate = finish_preparation(
                candidate,
                &secrets,
                policy,
                row.get("now"),
                payload,
                row.get("previous_expiry"),
                false,
            )?;
            output[usize::try_from(row.get::<i64, _>("idx"))?].push(candidate);
        }
        Ok(output)
    })
    .await?
}

pub async fn commit(
    tx: &mut Transaction<'_, Postgres>,
    groups: Vec<Vec<PreparedIdentity>>,
) -> Result<Vec<Vec<(String, Option<i64>)>>> {
    let mut seen = BTreeSet::new();
    let mut revisions = Vec::new();
    for group in &groups {
        let first = group
            .first()
            .context("identity preparation has no CA version")?;
        ensure!(seen.insert(first.id.clone()), "duplicate prepared identity");
        ensure!(
            group.iter().all(|c| c.id == first.id
                && c.user_id == first.user_id
                && c.ca_id == first.ca_id
                && c.expected_identity_revision == first.expected_identity_revision
                && c.authorization_revision == first.authorization_revision
                && c.user_revision == first.user_revision
                && c.policy_revision == first.policy_revision),
            "batch signing dependencies do not describe one stable identity"
        );
        revisions.push((
            first.expected_identity_revision,
            first.authorization_revision,
        ));
    }
    let rounds = groups.iter().map(Vec::len).max().unwrap_or(0);
    let mut groups: Vec<Vec<Option<PreparedIdentity>>> = groups
        .into_iter()
        .map(|g| g.into_iter().map(Some).collect())
        .collect();
    let mut output = vec![Vec::new(); groups.len()];
    // Process CA versions in rounds: later versions observe revisions published
    // by earlier versions, exactly as commit_versions does for a single identity.
    for round in 0..rounds {
        for start in (0..groups.len()).step_by(PAGE_SIZE) {
            let mut positions = Vec::new();
            let mut candidates = Vec::new();
            for index in start..(start + PAGE_SIZE).min(groups.len()) {
                if let Some(candidate) = groups[index].get_mut(round).and_then(Option::take) {
                    let mut candidate = candidate;
                    if round > 0 {
                        candidate.expected_identity_revision = revisions[index].0;
                        candidate.authorization_revision = revisions[index].1;
                    }
                    revisions[index] = (
                        Some(
                            candidate.expected_identity_revision.unwrap_or(1)
                                + i64::from(candidate.issued.is_some()),
                        ),
                        Some(candidate.authorization_revision.unwrap_or(1)),
                    );
                    positions.push(index);
                    candidates.push(candidate);
                }
            }
            if candidates.is_empty() {
                continue;
            }
            let committed = commit_page(tx, candidates).await?;
            for (index, value) in positions.into_iter().zip(committed) {
                output[index].push(value);
            }
        }
    }
    Ok(output)
}

async fn commit_page(
    tx: &mut Transaction<'_, Postgres>,
    candidates: Vec<PreparedIdentity>,
) -> Result<Vec<(String, Option<i64>)>> {
    let requests:Vec<_>=candidates.iter().enumerate().map(|(idx,c)|json!({"idx":idx,"user_id":c.user_id,"ca_id":c.ca_id,"ca_version":c.ca_version,"id":c.id,"existing_version":c.existing_version})).collect();
    let rows=sqlx::query(r#"
WITH moment AS MATERIALIZED (SELECT clock_timestamp() AS now)
SELECT r.idx,m.now,a.policy_revision,g.state,ca.archived,u.revision AS user_revision,
 u.enabled AND (u.expires_at IS NULL OR u.expires_at>m.now) AND (u.quota_bytes IS NULL OR u.usage_bytes<u.quota_bytes) AS eligible,
 i.id AS identity_id,i.revision AS identity_revision,i.authorization_revision,i.credential_id,
 i.next_action_at AS identity_next_action,i.maintenance_error,i.scheduled_ca_version,c.latest_version,
 coalesce(NOT v.discarded AND v.not_before<=m.now AND v.expires_at>m.now,false) AS existing_usable,v.next_action_at AS existing_next_action
FROM jsonb_to_recordset($1) AS r(idx bigint,user_id text,ca_id text,ca_version bigint,id text,existing_version bigint)
CROSS JOIN moment m JOIN users u ON u.id=r.user_id
JOIN mtls_authorities a ON a.credential_id=r.ca_id JOIN credentials ca ON ca.id=r.ca_id
JOIN mtls_ca_generations g ON g.ca_id=r.ca_id AND g.ca_version=r.ca_version
LEFT JOIN mtls_user_identities i ON i.user_id=r.user_id AND i.ca_id=r.ca_id
LEFT JOIN credentials c ON c.id=i.credential_id
LEFT JOIN mtls_user_versions v ON v.identity_id=r.id AND v.credential_version=r.existing_version
ORDER BY r.idx FOR KEY SHARE OF g
"#).bind(json!(requests)).fetch_all(&mut **tx).await?;
    ensure!(
        rows.len() == candidates.len(),
        "user or CA disappeared during signing; retry"
    );
    let mut new_identities = Vec::new();
    let mut new_credentials = Vec::new();
    let mut materials = Vec::new();
    let mut credential_updates = Vec::new();
    let mut identity_updates = Vec::new();
    let mut versions = Vec::new();
    let mut output = Vec::new();
    for (c, row) in candidates.into_iter().zip(rows) {
        let now: DateTime<Utc> = row.get("now");
        ensure!(
            !row.get::<bool, _>("archived")
                && row.get::<i64, _>("policy_revision") == c.policy_revision
                && row.get::<String, _>("state") == c.generation_state,
            "CA changed during signing; retry"
        );
        ensure!(
            row.get::<i64, _>("user_revision") == c.user_revision,
            "user changed during signing; retry"
        );
        ensure!(
            c.issued.is_none() || row.get::<bool, _>("eligible"),
            "user became ineligible during signing; retry"
        );
        let existing_id: Option<String> = row.get("identity_id");
        ensure!(
            existing_id.as_ref().is_none_or(|id| id == &c.id)
                && row.get::<Option<i64>, _>("identity_revision") == c.expected_identity_revision
                && row.get::<Option<i64>, _>("authorization_revision") == c.authorization_revision,
            "shared identity changed during signing; retry"
        );
        if existing_id.is_none() {
            new_identities.push(json!({"id":c.id,"user_id":c.user_id,"ca_id":c.ca_id,"now":now}));
        }
        let published = c.generation_state == "published";
        let mut credential: Option<String> = row.get("credential_id");
        let mut next_action: Option<DateTime<Utc>> = row.get("identity_next_action");
        let mut maintenance: Option<String> = row.get("maintenance_error");
        let mut scheduled: Option<i64> = row.get("scheduled_ca_version");
        if published {
            scheduled = Some(c.ca_version);
        }
        let issued = c.issued.is_some();
        let result_version;
        if let Some(leaf) = c.issued {
            ensure!(leaf.expires_at > now, "prepared certificate expired; retry");
            let (payload, metadata, new_id) = c
                .prepared_material
                .context("prepared identity material is missing")?;
            let version = if let Some(id) = &credential {
                let version = row.get::<i64, _>("latest_version") + 1;
                credential_updates.push(json!({"id":id,"version":version,"now":now}));
                version
            } else {
                new_credentials.push(json!({"id":new_id,"name":format!("Managed mTLS · {}",c.id),"user_id":c.user_id,"now":now}));
                credential = Some(new_id);
                1
            };
            let id = credential.as_ref().expect("issued credential");
            materials.push(json!({"credential_id":id,"version":version,"payload_enc":payload,"metadata":metadata,"now":now}));
            versions.push(json!({"identity_id":c.id,"credential_id":id,"credential_version":version,"ca_id":c.ca_id,"ca_version":c.ca_version,
                "not_before":leaf.not_before,"expires_at":leaf.expires_at,"next_action_at":leaf.next_action_at,"released":published,"now":now}));
            if published {
                let blocked = leaf.next_action_at <= now;
                next_action = Some(if blocked {
                    now + chrono::Duration::hours(1)
                } else {
                    leaf.next_action_at
                });
                maintenance = blocked
                    .then(|| "CA update required: cannot extend the existing certificate".into());
            }
            result_version = Some(version);
        } else {
            ensure!(
                c.existing_version.is_none() || row.get::<bool, _>("existing_usable"),
                "shared certificate expired or was discarded during preparation; retry"
            );
            if published {
                if c.needs_ca_update {
                    maintenance =
                        Some("CA update required: cannot extend the existing certificate".into());
                    next_action = Some(now + chrono::Duration::hours(1));
                } else {
                    next_action = row.get("existing_next_action");
                }
            }
            result_version = c.existing_version;
        }
        identity_updates.push(json!({"id":c.id,"credential_id":credential,"step":i64::from(issued),"next_action_at":next_action,"maintenance_error":maintenance,"scheduled_ca_version":scheduled,"now":now}));
        output.push((c.id, result_version));
    }
    if !new_identities.is_empty() {
        sqlx::query("INSERT INTO mtls_user_identities(id,user_id,ca_id,created_at,updated_at) SELECT id,user_id,ca_id,now,now FROM jsonb_to_recordset($1) AS r(id text,user_id text,ca_id text,now timestamptz) ORDER BY id").bind(json!(new_identities)).execute(&mut **tx).await?;
    }
    if !new_credentials.is_empty() {
        sqlx::query("INSERT INTO credentials(id,name,kind,owner_user_id,created_at,updated_at) SELECT id,name,'tls_identity',user_id,now,now FROM jsonb_to_recordset($1) AS r(id text,name text,user_id text,now timestamptz) ORDER BY id").bind(json!(new_credentials)).execute(&mut **tx).await?;
    }
    if !materials.is_empty() {
        sqlx::query("INSERT INTO credential_versions(credential_id,version,payload_enc,metadata,created_at) SELECT credential_id,version,payload_enc,metadata,now FROM jsonb_to_recordset($1) AS r(credential_id text,version bigint,payload_enc text,metadata jsonb,now timestamptz) ORDER BY credential_id,version").bind(json!(materials)).execute(&mut **tx).await?;
    }
    if !credential_updates.is_empty() {
        sqlx::query("UPDATE credentials c SET latest_version=r.version,revision=c.revision+1,archived=false,updated_at=r.now FROM jsonb_to_recordset($1) AS r(id text,version bigint,now timestamptz) WHERE c.id=r.id").bind(json!(credential_updates)).execute(&mut **tx).await?;
    }
    sqlx::query("UPDATE mtls_user_identities i SET credential_id=r.credential_id,revision=i.revision+r.step,archived=false,next_action_at=r.next_action_at,maintenance_error=r.maintenance_error,scheduled_ca_version=r.scheduled_ca_version,updated_at=r.now FROM jsonb_to_recordset($1) AS r(id text,credential_id text,step bigint,next_action_at timestamptz,maintenance_error text,scheduled_ca_version bigint,now timestamptz) WHERE i.id=r.id").bind(json!(identity_updates)).execute(&mut **tx).await?;
    if !versions.is_empty() {
        sqlx::query("INSERT INTO mtls_user_versions(identity_id,credential_id,credential_version,ca_id,ca_version,not_before,expires_at,next_action_at,released,created_at) SELECT identity_id,credential_id,credential_version,ca_id,ca_version,not_before,expires_at,next_action_at,released,now FROM jsonb_to_recordset($1) AS r(identity_id text,credential_id text,credential_version bigint,ca_id text,ca_version bigint,not_before timestamptz,expires_at timestamptz,next_action_at timestamptz,released bool,now timestamptz) ORDER BY identity_id,credential_version").bind(json!(versions)).execute(&mut **tx).await?;
    }
    Ok(output)
}

fn finish_preparation(
    mut candidate: PreparedIdentity,
    secrets: &SecretBox,
    policy: Policy,
    now: DateTime<Utc>,
    payload: Option<Value>,
    previous_expiry: Option<DateTime<Utc>>,
    force: bool,
) -> Result<PreparedIdentity> {
    if let Some(payload) = payload {
        let authority = Authority::from_payload(&payload, now)?;
        let issued = authority.sign_user(&Uuid::parse_str(&candidate.id)?, policy, now)?;
        ensure!(
            force
                || issued.expires_at < authority.expires_at - chrono::Duration::minutes(5)
                || previous_expiry.is_none_or(|previous| issued.expires_at > previous),
            "CA update required: cannot extend the existing certificate"
        );
        let payload = issued.material.payload();
        let mut metadata = crate::credentials::validate_payload("tls_identity", &payload)?;
        metadata["managed"] = json!(true);
        metadata["identity_id"] = json!(candidate.id);
        metadata["ca_id"] = json!(candidate.ca_id);
        metadata["ca_version"] = json!(candidate.ca_version);
        metadata["policy_revision"] = json!(candidate.policy_revision);
        candidate.prepared_material = Some((
            secrets.encrypt(&payload.to_string())?,
            metadata,
            Uuid::new_v4().to_string(),
        ));
        candidate.issued = Some(issued);
    }
    Ok(candidate)
}
