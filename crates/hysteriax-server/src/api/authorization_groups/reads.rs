//! Bounded management reads; mutations use indexed, durable delta tasks.
use super::*;
use axum::extract::Query;

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListQuery {
    page: Option<i64>,
    page_size: Option<i64>,
    q: Option<String>,
    sort: Option<String>,
    order: Option<String>,
}

impl ListQuery {
    fn validate(&self, fields: &[&str]) -> Result<(i64, i64, &str, &str), ApiError> {
        let page = self.page.unwrap_or(1);
        let size = self.page_size.unwrap_or(50);
        if page < 1 || !(1..=200).contains(&size) {
            return Err(ApiError::bad_request(
                "page must be positive and page_size must be between 1 and 200",
            ));
        }
        let sort = self.sort.as_deref().unwrap_or("name");
        if !fields.contains(&sort) {
            return Err(ApiError::bad_request(
                "invalid authorization group sort field",
            ));
        }
        let order = match self.order.as_deref().unwrap_or("asc") {
            "asc" => "ASC",
            "desc" => "DESC",
            _ => {
                return Err(ApiError::bad_request(
                    "invalid authorization group sort order",
                ));
            }
        };
        Ok((page, size, sort, order))
    }
    fn search(&self) -> &str {
        self.q.as_deref().unwrap_or("").trim()
    }
}

const GROUP_COLUMNS: &str = "g.id,g.name,g.revision,g.created_at,g.updated_at,(SELECT count(*) FROM authorization_group_users WHERE group_id=g.id) AS user_count,(SELECT count(*) FROM authorization_group_nodes WHERE group_id=g.id) AS node_count";

fn summary(row: &sqlx::postgres::PgRow) -> Value {
    json!({"id":row.get::<String,_>("id"),"name":row.get::<String,_>("name"),
        "revision":row.get::<i64,_>("revision"),"user_count":row.get::<i64,_>("user_count"),
        "node_count":row.get::<i64,_>("node_count"),"created_at":row.get::<DateTime<Utc>,_>("created_at"),
        "updated_at":row.get::<DateTime<Utc>,_>("updated_at")})
}

pub(crate) async fn list(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    let (requested, size, sort, order) = query.validate(&["name", "created_at", "updated_at"])?;
    let filter =
        "($1 = '' OR strpos(lower(g.name),lower($1))>0 OR strpos(lower(g.id),lower($1))>0)";
    let mut tx = crate::read_snapshot::begin(&state.pool).await?;
    let total: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT count(*) FROM authorization_groups g WHERE {filter}"
    )))
    .bind(query.search())
    .fetch_one(&mut *tx)
    .await?;
    let page = requested.min(((total + size - 1) / size).max(1));
    let order_by = if sort == "name" {
        format!("lower(g.name) COLLATE \"C\" {order},g.name COLLATE \"C\" {order},g.id {order}")
    } else {
        format!("g.{sort} {order},g.id {order}")
    };
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT {GROUP_COLUMNS} FROM authorization_groups g WHERE {filter} ORDER BY {order_by} LIMIT $2 OFFSET $3")))
        .bind(query.search()).bind(size).bind((page-1)*size).fetch_all(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(
        json!({"items":rows.iter().map(summary).collect::<Vec<_>>(),"total":total,"page":page,"page_size":size}),
    ))
}

pub(crate) async fn get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let mut tx = crate::read_snapshot::begin(&state.pool).await?;
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {GROUP_COLUMNS} FROM authorization_groups g WHERE g.id=$1"
    )))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::not_found("authorization group"))?;
    tx.commit().await?;
    Ok(Json(summary(&row)))
}

#[derive(Default, Deserialize)]
pub(crate) struct MembersQuery {
    page: Option<i64>,
    page_size: Option<i64>,
    q: Option<String>,
    sort: Option<String>,
    order: Option<String>,
    member: Option<bool>,
    enabled: Option<bool>,
}

pub(crate) async fn list_users(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<MembersQuery>,
) -> Result<Json<Value>, ApiError> {
    members(&state, &id, query, true).await
}
pub(crate) async fn list_nodes(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<MembersQuery>,
) -> Result<Json<Value>, ApiError> {
    members(&state, &id, query, false).await
}

async fn members(
    state: &AppState,
    id: &str,
    query: MembersQuery,
    users: bool,
) -> Result<Json<Value>, ApiError> {
    // Keep typed query fields flat: serde_urlencoded cannot decode numeric values
    // through serde's flattened content deserializer.
    let list = ListQuery {
        page: query.page,
        page_size: query.page_size,
        q: query.q.clone(),
        sort: query.sort.clone(),
        order: query.order.clone(),
    };
    let fields: &[&str] = if users {
        &["name", "id", "expires_at"]
    } else {
        &["name", "id", "state"]
    };
    let (requested, size, sort, order) = list.validate(fields)?;
    if !users && query.enabled.is_some() {
        return Err(ApiError::bad_request("enabled only applies to users"));
    }
    let (table, relation, key) = if users {
        ("users", "authorization_group_users", "user_id")
    } else {
        ("nodes", "authorization_group_nodes", "node_id")
    };
    let mut tx = crate::read_snapshot::begin(&state.pool).await?;
    let revision: i64 = sqlx::query_scalar("SELECT revision FROM authorization_groups WHERE id=$1")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| ApiError::not_found("authorization group"))?;
    // EXISTS uses the relation's (group_id, entity_id) primary key and never duplicates rows.
    let membership =
        format!("EXISTS(SELECT 1 FROM {relation} m WHERE m.group_id=$1 AND m.{key}=e.id)");
    let status_filter = if users {
        " AND ($4::boolean IS NULL OR e.enabled=$4)"
    } else {
        " AND $4::boolean IS NULL"
    };
    let filter = format!(
        "({membership})=$3 AND ($2='' OR strpos(lower(e.name),lower($2))>0 OR strpos(lower(e.id),lower($2))>0){status_filter}"
    );
    let total: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT count(*) FROM {table} e WHERE {filter}"
    )))
    .bind(id)
    .bind(list.search())
    .bind(query.member.unwrap_or(true))
    .bind(query.enabled)
    .fetch_one(&mut *tx)
    .await?;
    let page = requested.min(((total + size - 1) / size).max(1));
    let order_by = if sort == "name" {
        format!("lower(e.name) COLLATE \"C\" {order},e.name COLLATE \"C\" {order},e.id {order}")
    } else {
        format!("e.{sort} {order} NULLS LAST,e.id {order}")
    };
    let extra = if users {
        "e.enabled,e.expires_at"
    } else {
        "e.state"
    };
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT e.id,e.name,{extra} FROM {table} e WHERE {filter} ORDER BY {order_by} LIMIT $5 OFFSET $6")))
        .bind(id).bind(list.search()).bind(query.member.unwrap_or(true)).bind(query.enabled).bind(size).bind((page-1)*size).fetch_all(&mut *tx).await?;
    tx.commit().await?;
    let items: Vec<_> = rows
        .iter()
        .map(|row| {
            let mut item =
                json!({"id":row.get::<String,_>("id"),"name":row.get::<String,_>("name")});
            if users {
                item["enabled"] = json!(row.get::<bool, _>("enabled"));
                item["expires_at"] = json!(row.get::<Option<DateTime<Utc>>, _>("expires_at"));
            } else {
                item["state"] = json!(row.get::<String, _>("state"));
            }
            item
        })
        .collect();
    Ok(Json(
        json!({"items":items,"total":total,"page":page,"page_size":size,"revision":revision}),
    ))
}

#[cfg(test)]
mod tests;
