//! HTTP surface of the MCP adapter: `/mcp` and `/connector/control`.
use super::{Adapter, handler::Handler};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request as HttpRequest, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response as HttpResponse},
    routing::post,
};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use serde_json::{Value, json};
use std::sync::Arc;
use windows_tools_core::{Control, Response, RpcError, Target};

pub fn router(adapter: Arc<Adapter>) -> Router {
    let handler = Handler {
        adapter: adapter.clone(),
    };
    let host = adapter
        .origin
        .strip_prefix("https://")
        .unwrap_or("")
        .to_owned();
    let service = StreamableHttpService::new(
        move || Ok(handler.clone()),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default()
            .with_legacy_session_mode(false)
            .with_json_response(true)
            .with_max_request_body_bytes(1024 * 1024)
            .with_allowed_hosts(vec![host])
            .with_allowed_origins(vec![adapter.origin.clone()]),
    );
    Router::new()
        .nest_service("/mcp", service)
        .route(
            "/connector/control",
            post(control).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .with_state(adapter.clone())
        .layer(middleware::from_fn_with_state(adapter, guard))
}

/// Browser Origins are refused, and every route needs the token.
async fn guard(
    State(adapter): State<Arc<Adapter>>,
    request: HttpRequest,
    next: Next,
) -> HttpResponse {
    if request
        .headers()
        .get(header::ORIGIN)
        .is_some_and(|v| v.to_str().ok() != Some(adapter.origin.as_str()))
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    match bearer(request.headers()) {
        None => challenge(None),
        Some(token) if !adapter.authenticated(token) => challenge(Some("invalid_token")),
        Some(_) => next.run(request).await,
    }
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
}

/// RFC 6750 challenge. The token is provisioned on the machine, so no OAuth resource metadata
/// is advertised.
fn challenge(error: Option<&str>) -> HttpResponse {
    let mut value = String::from(r#"Bearer realm="windows-tools""#);
    if let Some(error) = error {
        value.push_str(&format!(r#", error="{error}""#));
    }
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, value)],
    )
        .into_response()
}

/// A lease call, as a connector posts it.
#[derive(serde::Deserialize)]
struct Request {
    version: u32,
    id: String,
    method: String,
    target: Option<Target>,
    #[serde(default)]
    args: Value,
}

/// Lease lifecycle for the connector, outside the model's tool list.
async fn control(
    State(adapter): State<Arc<Adapter>>,
    headers: HeaderMap,
    Json(request): Json<Request>,
) -> Result<Json<Response>, StatusCode> {
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok());
    if host != adapter.origin.strip_prefix("https://") {
        return Err(StatusCode::BAD_REQUEST);
    }
    let valid = request.version == 1
        && !request.id.trim().is_empty()
        && request.id.len() <= 128
        && request.args == json!({});
    let result = match (valid, Control::named(&request.method), request.target) {
        (true, Some(control), Some(target)) => {
            tokio::task::spawn_blocking(move || adapter.tools.control(control, &target))
                .await
                .unwrap_or_else(|_| {
                    Err(RpcError::unknown(
                        "internal_error",
                        "Control outcome is unknown; describe the session.",
                    ))
                })
        }
        _ => Err(RpcError::unsent(
            "invalid_arguments",
            "Connector control requires version 1, a request ID, a selected target, empty args and a lifecycle method.",
        )),
    };
    Ok(Json(Response::new(request.id, result)))
}
