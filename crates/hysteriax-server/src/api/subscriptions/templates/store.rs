use super::{catalog, document::*, render};
use crate::{db, error::ApiError, security::SecretBox, state::AppState};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction, postgres::PgRow};
use uuid::Uuid;

pub async fn initialize(pool: &sqlx::PgPool, secrets: &SecretBox) -> Result<(), ApiError> {
    let mut tx = db::begin_write(pool).await?;
    let initialized: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM subscription_template_initialization)")
            .fetch_one(&mut *tx)
            .await?;
    if !initialized {
        for format in ["mihomo", "singbox"] {
            let document = render::single_seed(format);
            ensure_valid(&validate(format, &document)?)?;
            let encrypted = secrets
                .encrypt(&serde_json::to_string(&document).map_err(|_| ApiError::internal())?)?;
            let id = format!("default-{format}");
            sqlx::query("INSERT INTO subscription_templates(id,name,format,draft_enc,is_default,published_version) VALUES($1,$2,$3,$4,TRUE,1)")
                .bind(&id).bind(format!("Default {format}")).bind(format).bind(&encrypted).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO subscription_template_versions(template_id,version,document_enc) VALUES($1,1,$2)").bind(&id).bind(&encrypted).execute(&mut *tx).await?;
        }
        sqlx::query("INSERT INTO subscription_template_initialization(id) VALUES(1)")
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}
fn metadata(row: &PgRow) -> Value {
    json!({"id":row.get::<String,_>("id"),"name":row.get::<String,_>("name"),"format":row.get::<String,_>("format"),"revision":row.get::<i64,_>("revision"),"archived":row.get::<bool,_>("archived"),"is_default":row.get::<bool,_>("is_default"),"published_version":row.get::<Option<i64>,_>("published_version"),"updated_at":row.get::<chrono::DateTime<chrono::Utc>,_>("updated_at")})
}
fn decode(secrets: &SecretBox, cipher: &str) -> Result<Document, ApiError> {
    serde_json::from_str(&secrets.decrypt(cipher)?).map_err(|_| ApiError::internal())
}
async fn row(state: &AppState, id: &str) -> Result<PgRow, ApiError> {
    sqlx::query("SELECT * FROM subscription_templates WHERE id=$1")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| ApiError::not_found("template"))
}
async fn locked(
    tx: &mut Transaction<'_, Postgres>,
    id: &str,
    revision: i64,
) -> Result<PgRow, ApiError> {
    let row = sqlx::query("SELECT * FROM subscription_templates WHERE id=$1 FOR UPDATE")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| ApiError::not_found("template"))?;
    if row.get::<i64, _>("revision") != revision {
        return Err(ApiError::conflict(
            "template changed; local edits must be retained and compared before retrying",
        ));
    }
    if row.get::<bool, _>("archived") {
        return Err(ApiError::conflict("template is archived"));
    }
    Ok(row)
}
async fn audit(
    tx: &mut Transaction<'_, Postgres>,
    id: &str,
    action: &str,
    revision: i64,
    version: Option<i64>,
) -> Result<(), ApiError> {
    audit_with_source(tx, id, action, revision, version, None).await
}
async fn audit_with_source(
    tx: &mut Transaction<'_, Postgres>,
    id: &str,
    action: &str,
    revision: i64,
    version: Option<i64>,
    source_version: Option<i64>,
) -> Result<(), ApiError> {
    sqlx::query("INSERT INTO audit_records(id,actor,action,entity_type,entity_id,detail_json,created_at) VALUES($1,$2,$3,'subscription_template',$4,$5,now())")
        .bind(Uuid::new_v4().to_string()).bind(crate::api::audit_actor()).bind(action).bind(id).bind(json!({"revision":revision,"version":version,"source_version":source_version,"result":"success"})).execute(&mut **tx).await?;
    Ok(())
}
#[derive(Default, Deserialize)]
pub struct ListQuery {
    pub page: Option<i64>,
    pub page_size: Option<i64>,
    pub format: Option<String>,
    pub archived: Option<bool>,
}
pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    let page = query.page.unwrap_or(1);
    let size = query.page_size.unwrap_or(50);
    if page < 1 || !(1..=200).contains(&size) || page.checked_mul(size).is_none() {
        return Err(ApiError::bad_request("invalid pagination"));
    }
    let rows=sqlx::query("SELECT * FROM subscription_templates WHERE ($1::text IS NULL OR format=$1) AND archived=$2 ORDER BY name,id LIMIT $3 OFFSET $4")
        .bind(&query.format).bind(query.archived.unwrap_or(false)).bind(size).bind((page-1)*size).fetch_all(&state.pool).await?;
    let total:i64=sqlx::query_scalar("SELECT count(*) FROM subscription_templates WHERE ($1::text IS NULL OR format=$1) AND archived=$2").bind(&query.format).bind(query.archived.unwrap_or(false)).fetch_one(&state.pool).await?;
    Ok(Json(
        json!({"items":rows.iter().map(metadata).collect::<Vec<_>>(),"page":page,"page_size":size,"total":total}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Create {
    pub expected_revision: i64,
    pub name: String,
    pub format: String,
    pub document: Option<Document>,
}
fn name(value: &str) -> Result<&str, ApiError> {
    let n = value.trim();
    if n.is_empty() || n.chars().count() > 180 || n.chars().any(char::is_control) {
        return Err(ApiError::bad_request(
            "name must contain 1–180 printable characters",
        ));
    }
    Ok(n)
}
async fn create_inner(
    State(state): State<AppState>,
    Json(input): Json<Create>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if input.expected_revision != 0 || !["mihomo", "singbox"].contains(&input.format.as_str()) {
        return Err(ApiError::bad_request(
            "new templates require revision 0 and a supported format",
        ));
    }
    let name = name(&input.name)?;
    let document = input
        .document
        .unwrap_or_else(|| render::single_seed(&input.format));
    document.shape()?;
    let mut tx = db::begin_write(&state.pool).await?;
    let id = Uuid::new_v4().to_string();
    let row=sqlx::query("INSERT INTO subscription_templates(id,name,format,draft_enc) VALUES($1,$2,$3,$4) RETURNING *")
        .bind(&id).bind(name).bind(input.format).bind(state.secrets.encrypt(&serde_json::to_string(&document).map_err(|_|ApiError::internal())?)?).fetch_one(&mut *tx).await?;
    audit(&mut tx, &id, "template.created", 1, None).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(metadata(&row))))
}
pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(metadata(&row(&state, &id).await?)))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    pub expected_revision: i64,
    pub name: Option<String>,
    pub is_default: Option<bool>,
    pub archived: Option<bool>,
    pub replacement_id: Option<String>,
    pub replacement_revision: Option<i64>,
}
async fn patch_inner(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<Patch>,
) -> Result<Json<Value>, ApiError> {
    let mut tx = db::begin_write(&state.pool).await?;
    let current = locked(&mut tx, &id, input.expected_revision).await?;
    let format: String = current.get("format");
    let make_default = input.is_default.unwrap_or(false);
    if make_default
        && (input.archived == Some(true)
            || current.get::<Option<i64>, _>("published_version").is_none())
    {
        return Err(ApiError::bad_request(
            "default template must be active and published",
        ));
    }
    if input.is_default == Some(false) && current.get::<bool, _>("is_default") {
        return Err(ApiError::bad_request(
            "set another template as default instead",
        ));
    }
    if input.archived == Some(true) && current.get::<bool, _>("is_default") {
        let replacement = input
            .replacement_id
            .as_deref()
            .filter(|s| *s != id)
            .ok_or_else(|| {
                ApiError::bad_request("default archive requires a different replacement template")
            })?;
        let candidate = locked(
            &mut tx,
            replacement,
            input
                .replacement_revision
                .ok_or_else(|| ApiError::bad_request("replacement_revision is required"))?,
        )
        .await?;
        if candidate.get::<String, _>("format") != format
            || candidate
                .get::<Option<i64>, _>("published_version")
                .is_none()
        {
            return Err(ApiError::bad_request(
                "replacement must have the same format and be published",
            ));
        }
        sqlx::query("UPDATE subscription_templates SET is_default=FALSE WHERE id=$1")
            .bind(&id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE subscription_templates SET is_default=TRUE,revision=revision+1,updated_at=now() WHERE id=$1").bind(replacement).execute(&mut *tx).await?;
        audit(
            &mut tx,
            replacement,
            "template.default_changed",
            candidate.get::<i64, _>("revision") + 1,
            candidate.get("published_version"),
        )
        .await?;
    }
    if make_default {
        let changed=sqlx::query("UPDATE subscription_templates SET is_default=FALSE,revision=revision+1,updated_at=now() WHERE format=$1 AND is_default AND id<>$2 RETURNING id,revision,published_version").bind(&format).bind(&id).fetch_all(&mut *tx).await?;
        for old in changed {
            audit(
                &mut tx,
                &old.get::<String, _>("id"),
                "template.default_changed",
                old.get("revision"),
                old.get("published_version"),
            )
            .await?;
        }
    }
    let row=sqlx::query("UPDATE subscription_templates SET name=coalesce($2,name),is_default=CASE WHEN $3 THEN TRUE WHEN $4 THEN FALSE ELSE is_default END,archived=coalesce($5,archived),revision=revision+1,updated_at=now() WHERE id=$1 RETURNING *")
        .bind(&id).bind(input.name.as_deref().map(name).transpose()?).bind(make_default).bind(input.archived==Some(true)).bind(input.archived).fetch_one(&mut *tx).await?;
    audit(
        &mut tx,
        &id,
        if input.archived == Some(true) {
            "template.archived"
        } else if make_default {
            "template.default_changed"
        } else {
            "template.metadata_updated"
        },
        row.get("revision"),
        row.get("published_version"),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(metadata(&row)))
}
pub async fn draft(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let row = row(&state, &id).await?;
    let document = decode(&state.secrets, &row.get::<String, _>("draft_enc"))?;
    Ok(no_store(
        json!({"template":metadata(&row),"document":document}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Save {
    pub expected_revision: i64,
    pub document: Document,
}
async fn save_inner(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<Save>,
) -> Result<Json<Value>, ApiError> {
    input.document.shape()?;
    let mut tx = db::begin_write(&state.pool).await?;
    locked(&mut tx, &id, input.expected_revision).await?;
    let row=sqlx::query("UPDATE subscription_templates SET draft_enc=$2,revision=revision+1,updated_at=now() WHERE id=$1 RETURNING *").bind(&id).bind(state.secrets.encrypt(&serde_json::to_string(&input.document).map_err(|_|ApiError::internal())?)?).fetch_one(&mut *tx).await?;
    audit(
        &mut tx,
        &id,
        "template.draft_saved",
        row.get("revision"),
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(Json(metadata(&row)))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Validate {
    pub document: Option<Document>,
}
pub async fn validate_draft(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<Validate>,
) -> Result<Response, ApiError> {
    let row = row(&state, &id).await?;
    let document = input
        .document
        .unwrap_or(decode(&state.secrets, &row.get::<String, _>("draft_enc"))?);
    Ok(no_store(
        json!({"diagnostics":validate(&row.get::<String,_>("format"),&document)?}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Revision {
    pub expected_revision: i64,
}
async fn publish_document(
    state: &AppState,
    id: &str,
    revision: i64,
    version: Option<i64>,
) -> Result<Json<Value>, ApiError> {
    let mut tx = db::begin_write(&state.pool).await?;
    let current = locked(&mut tx, id, revision).await?;
    let format: String = current.get("format");
    let cipher = if let Some(version) = version {
        sqlx::query_scalar::<_,String>("SELECT document_enc FROM subscription_template_versions WHERE template_id=$1 AND version=$2").bind(id).bind(version).fetch_optional(&mut *tx).await?.ok_or_else(||ApiError::not_found("template version"))?
    } else {
        current.get("draft_enc")
    };
    let document = decode(&state.secrets, &cipher)?;
    let diagnostics = validate(&format, &document)?;
    ensure_valid(&diagnostics)?;
    // Use the exact production renderer for both empty and representative node sets on every platform.
    super::verify_rendering(&format, &document)?;
    let next:i64=sqlx::query_scalar("SELECT coalesce(max(version),0)+1 FROM subscription_template_versions WHERE template_id=$1").bind(id).fetch_one(&mut *tx).await?;
    sqlx::query("INSERT INTO subscription_template_versions(template_id,version,document_enc) VALUES($1,$2,$3)").bind(id).bind(next).bind(&cipher).execute(&mut *tx).await?;
    let row=sqlx::query("UPDATE subscription_templates SET published_version=$2,draft_enc=$3,revision=revision+1,updated_at=now() WHERE id=$1 RETURNING *").bind(id).bind(next).bind(&cipher).fetch_one(&mut *tx).await?;
    audit_with_source(
        &mut tx,
        id,
        if version.is_some() {
            "template.rolled_back"
        } else {
            "template.published"
        },
        row.get("revision"),
        Some(next),
        version,
    )
    .await?;
    tx.commit().await?;
    Ok(Json(metadata(&row)))
}
async fn publish_inner(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<Revision>,
) -> Result<Json<Value>, ApiError> {
    publish_document(&state, &id, input.expected_revision, None).await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rollback {
    pub expected_revision: i64,
    pub version: i64,
}
async fn rollback_inner(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<Rollback>,
) -> Result<Json<Value>, ApiError> {
    publish_document(&state, &id, input.expected_revision, Some(input.version)).await
}
pub async fn versions(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    row(&state, &id).await?;
    let page = query.page.unwrap_or(1);
    let size = query.page_size.unwrap_or(50);
    if page < 1 || !(1..=200).contains(&size) || page.checked_mul(size).is_none() {
        return Err(ApiError::bad_request("invalid pagination"));
    }
    let rows=sqlx::query("SELECT version,created_at FROM subscription_template_versions WHERE template_id=$1 ORDER BY version DESC LIMIT $2 OFFSET $3").bind(&id).bind(size).bind((page-1)*size).fetch_all(&state.pool).await?;
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM subscription_template_versions WHERE template_id=$1",
    )
    .bind(&id)
    .fetch_one(&state.pool)
    .await?;
    Ok(Json(
        json!({"items":rows.iter().map(|r|json!({"version":r.get::<i64,_>("version"),"created_at":r.get::<chrono::DateTime<chrono::Utc>,_>("created_at")})).collect::<Vec<_>>(),"page":page,"page_size":size,"total":total}),
    ))
}
#[derive(Deserialize)]
pub struct FormatQuery {
    pub format: String,
}
pub async fn fields(Query(input): Query<FormatQuery>) -> Result<Json<Value>, ApiError> {
    if !["mihomo", "singbox"].contains(&input.format.as_str()) {
        return Err(ApiError::bad_request("unsupported format"));
    }
    Ok(Json(catalog::catalog(&input.format)))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub format: String,
    pub source: Option<String>,
    pub config: Option<Value>,
}
pub async fn source(Json(input): Json<Source>) -> Result<Response, ApiError> {
    let config = match (&input.source, &input.config) {
        (Some(source), None) => parse(&input.format, source)?,
        (None, Some(config)) if config.is_object() => config.clone(),
        _ => {
            return Err(ApiError::bad_request(
                "supply either source or config object",
            ));
        }
    };
    Ok(no_store(
        json!({"source":serialize(&input.format,&config)?,"config":config}),
    ))
}
pub async fn export(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let row = row(&state, &id).await?;
    let document = decode(&state.secrets, &row.get::<String, _>("draft_enc"))?;
    Ok(no_store(
        json!({"package_version":1,"name":row.get::<String,_>("name"),"format":row.get::<String,_>("format"),"document":document}),
    ))
}
pub(crate) async fn published(
    state: &AppState,
    id: Option<&str>,
    format: Option<&str>,
) -> Result<(String, i64, String, Document), ApiError> {
    let row=sqlx::query("SELECT t.id,t.format,t.published_version,v.document_enc FROM subscription_templates t JOIN subscription_template_versions v ON v.template_id=t.id AND v.version=t.published_version WHERE NOT t.archived AND (($1::text IS NOT NULL AND t.id=$1) OR ($1::text IS NULL AND t.is_default AND t.format=$2))")
        .bind(id).bind(format).fetch_optional(&mut *crate::read_snapshot::begin(&state.pool).await?).await?.ok_or_else(||ApiError::not_found("published template"))?;
    Ok((
        row.get("id"),
        row.get("published_version"),
        row.get("format"),
        decode(&state.secrets, &row.get::<String, _>("document_enc"))?,
    ))
}
fn no_store(value: Value) -> Response {
    let mut response = Json(value).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preview {
    pub user_id: String,
    pub platform: String,
    pub kernel_version: Option<String>,
    #[serde(default)]
    pub reveal_secrets: bool,
    pub document: Option<Document>,
}
pub async fn preview(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<Preview>,
) -> Result<Response, ApiError> {
    let current = row(&state, &id).await?;
    let format: String = current.get("format");
    let document = input.document.unwrap_or(decode(
        &state.secrets,
        &current.get::<String, _>("draft_enc"),
    )?);
    super::super::validate_user(&state, &input.user_id).await?;
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("platform", &input.platform)
        .finish();
    let platform = super::super::formats::Platform::requested(Some(&query))?
        .ok_or_else(|| ApiError::bad_request("explicit preview platform required"))?;
    let client = super::super::formats::ClientInfo::detect(&format!(
        "{}/{}",
        if format == "singbox" {
            "sing-box"
        } else {
            "mihomo"
        },
        input.kernel_version.as_deref().unwrap_or("")
    ));
    if input.kernel_version.is_some() && client.version.is_none() {
        return Err(ApiError::bad_request("invalid stable kernel version"));
    }
    let client = super::super::formats::ClientInfo { platform, ..client };
    let eligible = super::super::eligible_nodes(&state, &input.user_id).await?;
    let nodes =
        super::super::load_subscription_nodes_eligible(&state, &input.user_id, Some(&eligible))
            .await?;
    let mut output = render::render(&format, &document, &nodes, &client)?;
    if !input.reveal_secrets {
        redact(&mut output["config"]);
    }
    output["source"] = json!(serialize(&format, &output["config"])?);
    output["redacted"] = json!(!input.reveal_secrets);
    Ok(no_store(output))
}
fn redact(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if [
                    "password",
                    "private-key",
                    "private_key",
                    "token",
                    "secret",
                    "obfs-password",
                    "certificate",
                    "client_certificate",
                    "client_key",
                    "key",
                    "users",
                    "authentication",
                    "headers",
                    "api_key",
                    "api-key",
                    "authorization",
                ]
                .contains(&key.as_str())
                {
                    *value = json!("[REDACTED]");
                } else {
                    redact(value);
                }
            }
        }
        Value::Array(items) => {
            for value in items {
                redact(value);
            }
        }
        _ => (),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Import {
    pub format: String,
    pub source: Option<String>,
    pub package: Option<Value>,
}
pub async fn import(Json(input): Json<Import>) -> Result<Response, ApiError> {
    if !["mihomo", "singbox"].contains(&input.format.as_str()) {
        return Err(ApiError::bad_request("unsupported format"));
    }
    if input.source.is_some() && input.package.is_some() {
        return Err(ApiError::bad_request(
            "supply native source or a template package, not both",
        ));
    }
    let inferred = input
        .source
        .as_deref()
        .and_then(|source| super::native_source::jsonc(source).ok())
        .filter(|value| {
            value.get("package_version").is_some()
                && value.get("document").is_some()
                && value.get("format").is_some()
        });
    if let Some(package) = input.package.or(inferred) {
        if package["package_version"] != 1 || package["format"] != input.format {
            return Err(ApiError::bad_request("invalid template package or format"));
        }
        let document: Document = serde_json::from_value(package["document"].clone())
            .map_err(|e| ApiError::bad_request(e.to_string()))?;
        document.shape()?;
        return Ok(no_store(
            json!({"diagnostics":validate(&input.format,&document)?,"document":document,"removed_nodes":[],"reference_mapping":[]}),
        ));
    }
    let mut common = parse(
        &input.format,
        input
            .source
            .as_deref()
            .ok_or_else(|| ApiError::bad_request("source or package is required"))?,
    )?;
    let mut removed = Vec::new();
    let mut mapping = Vec::new();
    let mut injections = Vec::new();
    let mut removed_providers = Vec::new();
    if input.format == "mihomo" {
        removed = common
            .as_object_mut()
            .unwrap()
            .remove("proxies")
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default();
        common["proxies"] = json!([]);
        if let Some(providers) = common
            .as_object_mut()
            .unwrap()
            .remove("proxy-providers")
            .and_then(|v| v.as_object().cloned())
        {
            removed_providers = providers.keys().cloned().collect();
        }
    } else if let Some(outbounds) = common["outbounds"].as_array_mut() {
        outbounds.retain(|v| {
            let keep = ["direct", "selector", "urltest", "block", "dns"]
                .contains(&v["type"].as_str().unwrap_or(""));
            if !keep {
                removed.push(v.clone());
            }
            keep
        });
    }
    if input.format == "singbox"
        && let Some(endpoints) = common
            .as_object_mut()
            .unwrap()
            .remove("endpoints")
            .and_then(|v| v.as_array().cloned())
    {
        removed.extend(endpoints);
    }
    let (groups_key, name_key, entries_key) = if input.format == "mihomo" {
        ("proxy-groups", "name", "proxies")
    } else {
        ("outbounds", "tag", "outbounds")
    };
    let names: Vec<String> = removed
        .iter()
        .filter_map(|n| n[name_key].as_str().map(str::to_owned))
        .collect();
    if let Some(groups) = common[groups_key].as_array_mut() {
        for group in groups {
            let mut provider_refs = Vec::new();
            if input.format == "mihomo"
                && let Some(map) = group.as_object_mut()
            {
                if let Some(uses) = map.remove("use").and_then(|v| v.as_array().cloned()) {
                    provider_refs.extend(uses.iter().filter_map(Value::as_str).map(str::to_owned));
                }
                for flag in [
                    "include-all",
                    "include-all-proxies",
                    "include-all-providers",
                ] {
                    if map.remove(flag) == Some(json!(true)) {
                        provider_refs.push(flag.into());
                    }
                }
                if !provider_refs.is_empty() {
                    map.entry(entries_key).or_insert(json!([]));
                }
            }
            let mut refs = Vec::new();
            if let Some(entries) = group.get_mut(entries_key).and_then(Value::as_array_mut) {
                refs = entries
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|s| names.iter().any(|n| n == s))
                    .map(str::to_owned)
                    .collect();
                entries.retain(|v| !v.as_str().is_some_and(|s| names.iter().any(|n| n == s)));
            }
            if !refs.is_empty() || !provider_refs.is_empty() {
                let name = group[name_key].as_str().unwrap_or("").to_string();
                mapping.push(json!({"group":name,"removed_references":refs,"removed_provider_references":provider_refs,"replacement":"authorized dynamic nodes"}));
                injections.push(Injection {
                    group: name,
                    ids: vec![],
                    contains: String::new(),
                    prefix: String::new(),
                    exclude: vec![],
                    descending: false,
                    preferred_prefix: String::new(),
                    on_empty: EmptyPolicy::Direct,
                });
            }
        }
    }
    let document = Document {
        schema_version: 2,
        common,
        platforms: Default::default(),
        injections,
    };
    let platform_moves: Vec<Value> = vec![];
    // Do not return credentials from removed nodes in import analysis.
    let removed: Vec<_> = removed
        .iter()
        .map(|n| json!({"name":n[name_key],"type":n["type"]}))
        .collect();
    Ok(no_store(
        json!({"diagnostics":validate(&input.format,&document)?,"document":document,"removed_nodes":removed,"reference_mapping":mapping,"platform_moves":platform_moves,"removed_providers":removed_providers}),
    ))
}

pub async fn version(
    State(state): State<AppState>,
    Path((id, version)): Path<(String, i64)>,
) -> Result<Response, ApiError> {
    let current = row(&state, &id).await?;
    let cipher:Option<String>=sqlx::query_scalar("SELECT document_enc FROM subscription_template_versions WHERE template_id=$1 AND version=$2").bind(&id).bind(version).fetch_optional(&state.pool).await?;
    let document = decode(
        &state.secrets,
        &cipher.ok_or_else(|| ApiError::not_found("template version"))?,
    )?;
    Ok(no_store(
        json!({"template":metadata(&current),"version":version,"document":document}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Refactor {
    pub expected_revision: i64,
    pub document: Document,
    pub kind: String,
    pub old_name: String,
    pub new_name: String,
}
pub async fn refactor(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(mut input): Json<Refactor>,
) -> Result<Response, ApiError> {
    let current = row(&state, &id).await?;
    if current.get::<i64, _>("revision") != input.expected_revision {
        return Err(ApiError::conflict(
            "template changed; reload metadata before refactoring references",
        ));
    }
    let format: String = current.get("format");
    rename(
        &mut input.document,
        &format,
        &input.kind,
        &input.old_name,
        &input.new_name,
    )?;
    Ok(no_store(
        json!({"document":input.document,"diagnostics":validate(&format,&input.document)?}),
    ))
}

async fn report_failure<T>(
    state: &AppState,
    id: &str,
    action: &str,
    revision: i64,
    version: Option<i64>,
    result: Result<T, ApiError>,
) -> Result<T, ApiError> {
    if let Err(error) = &result {
        let recorded=async{
            let mut tx=db::begin_write(&state.pool).await?;
            sqlx::query("INSERT INTO audit_records(id,actor,action,entity_type,entity_id,detail_json,created_at) VALUES($1,$2,$3,'subscription_template',$4,$5,now())")
                .bind(Uuid::new_v4().to_string()).bind(crate::api::audit_actor()).bind(action).bind(id)
                .bind(json!({"result":"failure","expected_revision":revision,"version":version,"code":error.code,"status":error.status.as_u16()}))
                .execute(&mut *tx).await?;tx.commit().await?;Ok::<_,sqlx::Error>(())
        }.await;
        if recorded.is_err() {
            tracing::error!(
                action,
                "unable to record subscription template failure metadata"
            );
        }
    }
    result
}
pub async fn create(
    State(state): State<AppState>,
    Json(input): Json<Create>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let revision = input.expected_revision;
    let result = create_inner(State(state.clone()), Json(input)).await;
    report_failure(
        &state,
        "new",
        "template.create_failed",
        revision,
        None,
        result,
    )
    .await
}
pub async fn patch(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<Patch>,
) -> Result<Json<Value>, ApiError> {
    let revision = input.expected_revision;
    let action = if input.archived == Some(true) {
        "template.archive_failed"
    } else if input.is_default == Some(true) {
        "template.default_change_failed"
    } else {
        "template.metadata_update_failed"
    };
    let result = patch_inner(State(state.clone()), Path(id.clone()), Json(input)).await;
    report_failure(&state, &id, action, revision, None, result).await
}
pub async fn save(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<Save>,
) -> Result<Json<Value>, ApiError> {
    let revision = input.expected_revision;
    let result = save_inner(State(state.clone()), Path(id.clone()), Json(input)).await;
    report_failure(
        &state,
        &id,
        "template.draft_save_failed",
        revision,
        None,
        result,
    )
    .await
}
pub async fn publish(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<Revision>,
) -> Result<Json<Value>, ApiError> {
    let revision = input.expected_revision;
    let result = publish_inner(State(state.clone()), Path(id.clone()), Json(input)).await;
    report_failure(
        &state,
        &id,
        "template.publish_failed",
        revision,
        None,
        result,
    )
    .await
}
pub async fn rollback(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<Rollback>,
) -> Result<Json<Value>, ApiError> {
    let revision = input.expected_revision;
    let version = input.version;
    let result = rollback_inner(State(state.clone()), Path(id.clone()), Json(input)).await;
    report_failure(
        &state,
        &id,
        "template.rollback_failed",
        revision,
        Some(version),
        result,
    )
    .await
}
