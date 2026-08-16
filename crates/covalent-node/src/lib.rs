//! Local daemon API and embedded accessible console.

use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use covalent_protocol::{NodeStatus, PROTOCOL_VERSION};
use serde::Serialize;

const INDEX_HTML: &str = include_str!("../../../packaging/web/index.html");
const APP_CSS: &str = include_str!("../../../packaging/web/app.css");
const APP_JS: &str = include_str!("../../../packaging/web/app.js");

/// Immutable state exposed by foundation endpoints.
#[derive(Clone, Debug)]
pub struct AppState {
    /// Non-sensitive status for local clients.
    pub status: NodeStatus,
}

/// Builds the versioned local API and static console router.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/assets/app.css", get(css))
        .route("/assets/app.js", get(javascript))
        .route("/healthz", get(health))
        .route("/api/v1/status", get(status))
        .with_state(state)
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn css() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], APP_CSS)
}

async fn javascript() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        APP_JS,
    )
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Health {
    status: &'static str,
    protocol_version: u16,
}

async fn health() -> impl IntoResponse {
    (
        StatusCode::OK,
        axum::Json(Health {
            status: "ok",
            protocol_version: PROTOCOL_VERSION,
        }),
    )
}

async fn status(State(state): State<AppState>) -> axum::Json<NodeStatus> {
    axum::Json(state.status)
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use covalent_protocol::PlatformTier;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;

    fn test_state() -> AppState {
        AppState {
            status: NodeStatus {
                device_name: "Test node".to_owned(),
                protocol_version: PROTOCOL_VERSION,
                lan_discovery: false,
                platform_tier: PlatformTier::Tier1,
                state: "foundation".to_owned(),
            },
        }
    }

    #[tokio::test]
    async fn health_is_machine_readable() {
        let response = router(test_state())
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON");
        assert_eq!(json["status"], "ok");
        assert_eq!(json["protocolVersion"], PROTOCOL_VERSION);
    }

    #[tokio::test]
    async fn console_has_accessible_landmarks() {
        let response = router(test_state())
            .oneshot(
                Request::builder()
                    .uri("/")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        let html = String::from_utf8(bytes.to_vec()).expect("UTF-8");
        assert!(html.contains("<main"));
        assert!(html.contains("aria-live"));
    }
}
