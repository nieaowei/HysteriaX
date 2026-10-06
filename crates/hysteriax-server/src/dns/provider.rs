use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::{Client, Method};
use serde_json::{Value, json};

/// Errors deliberately exclude response bodies and credential-bearing request metadata.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("DNS provider transport failure")]
    Transport,
    #[error("DNS provider returned HTTP {status}")]
    Http { status: u16, retry_after: i64 },
    #[error("DNS provider returned an invalid response")]
    InvalidResponse,
}
impl ProviderError {
    pub fn retry_delay(&self) -> Option<i64> {
        match self {
            Self::Transport => Some(30),
            Self::Http {
                status,
                retry_after,
            } if (*status == 429 || *status >= 500) && *retry_after <= 86400 => {
                Some((*retry_after).max(30))
            }
            _ => None,
        }
    }
}

pub struct Cloudflare {
    client: Client,
    token: String,
    base: String,
}
impl Cloudflare {
    pub fn new(token: String) -> Result<Self> {
        Ok(Self {
            client: Client::builder()
                .timeout(Duration::from_secs(20))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            token,
            base: "https://api.cloudflare.com/client/v4".into(),
        })
    }
    #[cfg(test)]
    pub fn with_base(token: String, base: String) -> Result<Self> {
        let mut provider = Self::new(token)?;
        provider.base = base;
        Ok(provider)
    }
    async fn request(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Value> {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(&self.token);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await.map_err(|_| ProviderError::Transport)?;
        let status = response.status();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|h| h.to_str().ok())
            .and_then(|value| {
                value.parse::<i64>().ok().or_else(|| {
                    chrono::DateTime::parse_from_rfc2822(value)
                        .ok()
                        .map(|time| {
                            ((time.with_timezone(&chrono::Utc) - chrono::Utc::now())
                                .num_milliseconds()
                                .max(0)
                                + 999)
                                / 1000
                        })
                })
            })
            .unwrap_or(30);
        if !status.is_success() {
            return Err(ProviderError::Http {
                status: status.as_u16(),
                retry_after,
            }
            .into());
        }
        let data: Value = response
            .json()
            .await
            .map_err(|_| ProviderError::InvalidResponse)?;
        if data["success"] != true {
            return Err(ProviderError::InvalidResponse.into());
        }
        Ok(data)
    }
    async fn paginated(&self, path: &str) -> Result<Vec<Value>> {
        let mut values = Vec::new();
        let mut page = 1;
        loop {
            let data = self
                .request(
                    Method::GET,
                    &format!("{path}?per_page=100&page={page}"),
                    None,
                )
                .await?;
            let result = data["result"]
                .as_array()
                .context("DNS provider result is not a list")?;
            values.extend(result.iter().cloned());
            let total = data["result_info"]["total_pages"].as_u64().unwrap_or(1);
            if page >= total {
                break;
            }
            page += 1;
            if page > 10000 {
                return Err(ProviderError::InvalidResponse.into());
            }
        }
        Ok(values)
    }
    pub async fn zones(&self) -> Result<Vec<Value>> {
        self.paginated("/zones").await
    }
    pub async fn zone(&self, zone: &str) -> Result<Value> {
        Ok(self
            .request(Method::GET, &format!("/zones/{zone}"), None)
            .await?["result"]
            .clone())
    }
    pub async fn records(&self, zone: &str) -> Result<Vec<Value>> {
        self.paginated(&format!("/zones/{zone}/dns_records")).await
    }
    pub async fn record(&self, zone: &str, id: &str) -> Result<Option<Value>> {
        match self
            .request(
                Method::GET,
                &format!("/zones/{zone}/dns_records/{id}"),
                None,
            )
            .await
        {
            Ok(data) => Ok(Some(data["result"].clone())),
            Err(error)
                if error
                    .downcast_ref::<ProviderError>()
                    .is_some_and(|e| matches!(e, ProviderError::Http { status: 404, .. })) =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }
    pub async fn create(&self, zone: &str, desired: &Value, operation: &str) -> Result<Value> {
        let mut body = desired.clone();
        body["comment"] = json!(format!("HysteriaX operation:{operation}"));
        Ok(self
            .request(
                Method::POST,
                &format!("/zones/{zone}/dns_records"),
                Some(&body),
            )
            .await?["result"]
            .clone())
    }
    pub async fn update(&self, zone: &str, id: &str, desired: &Value) -> Result<Value> {
        Ok(self
            .request(
                Method::PATCH,
                &format!("/zones/{zone}/dns_records/{id}"),
                Some(desired),
            )
            .await?["result"]
            .clone())
    }
    pub async fn delete(&self, zone: &str, id: &str) -> Result<()> {
        match self
            .request(
                Method::DELETE,
                &format!("/zones/{zone}/dns_records/{id}"),
                None,
            )
            .await
        {
            Ok(_) => Ok(()),
            Err(error)
                if error
                    .downcast_ref::<ProviderError>()
                    .is_some_and(|e| matches!(e, ProviderError::Http { status: 404, .. })) =>
            {
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
}

/// Compare editable state without volatile provider timestamps or unrelated metadata.
pub fn editable(record: &Value) -> Value {
    json!({"type": record["type"], "name": record["name"], "content": record["content"],
        "ttl": record["ttl"], "proxied": record["proxied"].as_bool().unwrap_or(false)})
}

pub fn retry_delay(error: &anyhow::Error) -> Option<i64> {
    error.chain().find_map(|e| {
        e.downcast_ref::<ProviderError>()
            .and_then(ProviderError::retry_delay)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, http::StatusCode, routing::get};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[tokio::test]
    async fn pagination_and_redacted_errors() {
        let pages = Arc::new(AtomicUsize::new(0));
        let count = pages.clone();
        let app = Router::new().route("/zones", get(move || {
            let page = count.fetch_add(1, Ordering::SeqCst) + 1;
            async move { Json(json!({"success":true,"result":[{"id":page}],"result_info":{"total_pages":2}})) }
        })) .route("/zones/denied", get(|| async { (StatusCode::FORBIDDEN, "secret-provider-token") }))
            .route("/zones/limited", get(|| async {
                let deadline=(chrono::Utc::now()+chrono::Duration::hours(1)).to_rfc2822();
                (StatusCode::TOO_MANY_REQUESTS,[("retry-after",deadline)],"not logged")
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let provider = Cloudflare::with_base("secret-provider-token".into(), base).unwrap();
        assert_eq!(provider.zones().await.unwrap().len(), 2);
        let limited = provider.zone("limited").await.unwrap_err();
        assert!((3590..=3600).contains(&retry_delay(&limited).unwrap()));
        let error = provider.zone("denied").await.unwrap_err();
        assert!(!error.to_string().contains("secret-provider-token"));
        assert_eq!(retry_delay(&error), None);
        assert_eq!(
            retry_delay(
                &ProviderError::Http {
                    status: 429,
                    retry_after: 125
                }
                .into()
            ),
            Some(125)
        );
        assert_eq!(
            retry_delay(
                &ProviderError::Http {
                    status: 429,
                    retry_after: 3600
                }
                .into()
            ),
            Some(3600)
        );
        assert_eq!(
            retry_delay(
                &ProviderError::Http {
                    status: 429,
                    retry_after: 172800
                }
                .into()
            ),
            None
        );
        server.abort();
    }
}
