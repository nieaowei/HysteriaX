pub mod catalog;
pub mod document;
mod mihomo_rules;
mod native_source;
mod render;
mod runtime_checks;
mod store;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod transaction_tests;
use super::formats::{ClientInfo, Platform, SubscriptionNode};
use crate::error::ApiError;
use axum::{
    body::Body,
    http::{HeaderValue, header},
    response::Response,
};
use document::Document;
pub use runtime_checks::check_runtime;
pub(crate) use store::v2;
pub use store::*;

pub(super) fn response_from_nodes(
    id: &str,
    document: &Document,
    format: &str,
    nodes: &[SubscriptionNode],
    client: &ClientInfo,
) -> Result<Response, ApiError> {
    let output = render::render(format, document, nodes, client)?;
    let source = document::serialize(format, &output["config"])?;
    let mut response = Response::new(Body::from(source));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(if format == "mihomo" {
            "application/yaml; charset=utf-8"
        } else {
            "application/json; charset=utf-8"
        }),
    );
    response.headers_mut().insert(
        "x-hysteriax-template",
        HeaderValue::from_str(id).map_err(|_| ApiError::internal())?,
    );
    response.headers_mut().insert(
        "x-hysteriax-filtered-nodes",
        HeaderValue::from_str(
            &output["filtered_nodes"]
                .as_array()
                .map_or(0, Vec::len)
                .to_string(),
        )
        .unwrap(),
    );
    Ok(response)
}
pub(super) fn platform_name(platform: Platform) -> Option<&'static str> {
    match platform {
        Platform::Linux => Some("linux"),
        Platform::Macos => Some("macos"),
        Platform::Windows => Some("windows"),
        Platform::Android => Some("android"),
        Platform::Ios => Some("ios"),
        _ => None,
    }
}

fn verify_rendering(format: &str, document: &Document) -> Result<(), ApiError> {
    for platform in document::PLATFORMS {
        let query = format!("platform={platform}");
        let client = ClientInfo {
            platform: super::formats::Platform::requested(Some(&query))?.unwrap(),
            ..ClientInfo::default()
        };
        let node = super::build_node(
            "Representative",
            "validation-node",
            "example.invalid",
            443,
            None,
            None,
            false,
            "validation-only",
            None,
            None,
            None,
            None,
            &serde_json::json!({}),
        );
        let mut empty = document.clone();
        for rule in &mut empty.injections {
            rule.on_empty = document::EmptyPolicy::Direct;
        }
        render::render(format, &empty, &[], &client)?;
        render::render(format, &empty, &[node], &client)?;
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn render_nodes_for_test(
    format: &str,
    nodes: &[SubscriptionNode],
    client: &ClientInfo,
) -> Result<serde_json::Value, ApiError> {
    render::render(format, &render::seed(format), nodes, client)
}
#[cfg(test)]
pub(super) fn response_from_seed_for_test(
    format: &str,
    nodes: &[SubscriptionNode],
    client: &ClientInfo,
) -> Result<Response, ApiError> {
    response_from_nodes("seed-test", &render::seed(format), format, nodes, client)
}
