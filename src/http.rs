use crate::{app_log::AppLog, engine::Snapshot, model::BuildInfo};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, RwLock,
};
use subtle::ConstantTimeEq;
use uuid::Uuid;

#[derive(Clone)]
pub struct HttpState {
    pub snapshot: Arc<RwLock<Snapshot>>,
    pub build: BuildInfo,
    pub state_token: Arc<String>,
    pub state_enabled: bool,
    pub workers_ok: Arc<AtomicBool>,
    pub log: AppLog,
}
pub fn router(state: HttpState) -> Router {
    let mut router = Router::new()
        .route("/livez", get(livez))
        .route("/readyz", get(readyz));
    if state.state_enabled {
        router = router.route("/state", get(snapshot));
    }
    router.fallback(not_found).with_state(state)
}
fn correlation(headers: &HeaderMap) -> String {
    headers
        .get("x-correlation-id")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| {
            Uuid::parse_str(s)
                .ok()
                .filter(|id| id.hyphenated().to_string() == s)
                .map(|_| s.to_owned())
        })
        .unwrap_or_else(|| Uuid::new_v4().to_string())
}
fn response(status: StatusCode, mut body: Value, id: &str, no_store: bool) -> Response {
    body["correlation_id"] = json!(id);
    let mut result = (status, Json(body)).into_response();
    result
        .headers_mut()
        .insert("x-correlation-id", id.parse().expect("canonical UUID"));
    if no_store {
        result
            .headers_mut()
            .insert("cache-control", "no-store".parse().expect("static header"));
    }
    result
}
async fn livez(State(s): State<HttpState>, headers: HeaderMap) -> Response {
    let id = correlation(&headers);
    response(
        StatusCode::OK,
        json!({"ok":true,"probe":"liveness","service":"carstate","version":s.build.version,"buildHash":s.build.build_hash}),
        &id,
        false,
    )
}
async fn readyz(State(s): State<HttpState>, headers: HeaderMap) -> Response {
    let id = correlation(&headers);
    let start = std::time::Instant::now();
    let snapshot = s.snapshot.read().expect("snapshot lock");
    let mqtt = snapshot.mqtt["connected"]["value"] == true
        && snapshot.mqtt["subscriptions"]
            .as_array()
            .is_some_and(|subs| subs.iter().all(|v| v["active"]["value"] == true));
    let workers = s.workers_ok.load(Ordering::Relaxed)
        && snapshot.runtime["workers"]["evaluator"]["healthy"] == true;
    let control = snapshot.runtime["control_pipeline_healthy"] == true;
    let ok = mqtt && workers && control;
    let latency = start.elapsed().as_secs_f64() * 1000.;
    response(
        if ok {
            StatusCode::OK
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        },
        json!({"ok":ok,"probe":"readiness","service":"carstate","version":s.build.version,"buildHash":s.build.build_hash,"checks":{"mqtt":{"ok":mqtt,"description":"MQTT session and subscriptions available","latency_ms":latency},"workers":{"ok":workers,"description":"Required workers progressing","latency_ms":latency},"control":{"ok":control,"description":"Control delivery available","latency_ms":latency}}}),
        &id,
        false,
    )
}
async fn snapshot(State(s): State<HttpState>, headers: HeaderMap) -> Response {
    let id = correlation(&headers);
    let supplied = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if !s.state_token.trim().is_empty()
        && !bool::from(supplied.as_bytes().ct_eq(s.state_token.as_bytes()))
    {
        s.log.correlated(
            "warn",
            "HTTP_STATE_AUTH_REJECTED",
            "State authentication rejected",
            json!({}),
            Some(id.clone()),
        );
        let mut r = response(
            StatusCode::UNAUTHORIZED,
            json!({"ok":false,"error":"unauthorized"}),
            &id,
            true,
        );
        r.headers_mut()
            .insert("www-authenticate", "Bearer".parse().expect("static header"));
        return r;
    }
    let snapshot = s.snapshot.read().expect("snapshot lock").clone();
    let mut body = serde_json::to_value(snapshot).expect("allowlisted snapshot");
    if !s.workers_ok.load(Ordering::Relaxed) {
        body["runtime"]["ready"] = json!(false);
        body["runtime"]["control_pipeline_healthy"] = json!(false);
    }
    response(StatusCode::OK, body, &id, true)
}
async fn not_found(headers: HeaderMap) -> Response {
    response(
        StatusCode::NOT_FOUND,
        json!({"ok":false,"error":"not_found"}),
        &correlation(&headers),
        true,
    )
}
