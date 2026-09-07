mod common;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use carstate::{
    app_log::{self, AppLog, Identity},
    engine::SnapshotIdentity,
    http::{self, HttpState},
    model::{BuildInfo, Clock},
};
use common::*;
use http_body_util::BodyExt;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, RwLock,
    },
};
use tower::ServiceExt;
fn state(enabled: bool) -> HttpState {
    let e = engine();
    let build = BuildInfo::default();
    let snapshot = e.snapshot(
        0.,
        &Clock::default(),
        &SnapshotIdentity {
            build: build.clone(),
            client_id: "private-client-id".into(),
            generated: true,
            credentials: true,
        },
    );
    let (log, _) = AppLog::start(
        app_log::bootstrap(),
        Identity::new("test", &BTreeMap::new()),
    );
    HttpState {
        snapshot: Arc::new(RwLock::new(snapshot)),
        build,
        state_token: Arc::new("private-admin-token".into()),
        state_enabled: enabled,
        workers_ok: Arc::new(AtomicBool::new(true)),
        log,
    }
}
async fn request(
    s: HttpState,
    path: &str,
    auth: Option<&str>,
    correlation: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let mut request = Request::builder().uri(path);
    if let Some(auth) = auth {
        request = request.header("authorization", auth);
    }
    if let Some(id) = correlation {
        request = request.header("x-correlation-id", id);
    }
    let r = http::router(s)
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = r.status();
    let headers = r.headers().clone();
    let body = r.into_body().collect().await.unwrap().to_bytes();
    (status, headers, serde_json::from_slice(&body).unwrap())
}
#[tokio::test]
async fn probe_shapes_correlation_and_outage_behavior() {
    let s = state(false);
    for id in [
        None,
        Some("bad"),
        Some("0198f3f5-d9af-7a5b-8f1c-fcb2d40c8241"),
    ] {
        let (status, headers, body) = request(s.clone(), "/livez", None, id).await;
        assert_eq!(status, 200);
        assert_eq!(body["ok"], true);
        assert_eq!(body["service"], "carstate");
        assert_eq!(body["buildHash"], s.build.build_hash);
        assert_eq!(
            body["correlation_id"].as_str().unwrap(),
            headers["x-correlation-id"]
        );
        assert!(uuid::Uuid::parse_str(body["correlation_id"].as_str().unwrap()).is_ok());
        if id.is_some_and(|v| v != "bad") {
            assert_eq!(body["correlation_id"], id.unwrap());
        }
    }
    s.snapshot.write().unwrap().runtime["control_pipeline_healthy"] = false.into();
    s.snapshot.write().unwrap().mqtt["connected"]["value"] = false.into();
    let (status, _, body) = request(s.clone(), "/readyz", None, None).await;
    assert_eq!(status, 500);
    assert_eq!(body["checks"]["mqtt"]["ok"], false);
    assert!(!body.to_string().contains("private"));
    assert!(!body.to_string().contains("car/status"));
    assert_eq!(request(s.clone(), "/livez", None, None).await.0, 200);
    s.snapshot.write().unwrap().runtime["control_pipeline_healthy"] = true.into();
    s.snapshot.write().unwrap().mqtt["connected"]["value"] = true.into();
    assert_eq!(request(s.clone(), "/readyz", None, None).await.0, 200);
    s.workers_ok.store(false, Ordering::Relaxed);
    assert_eq!(request(s, "/readyz", None, None).await.0, 500);
}
#[tokio::test]
async fn state_is_default_off_and_auth_errors_reveal_no_snapshot() {
    assert_eq!(request(state(false), "/state", None, None).await.0, 404);
    for auth in [
        None,
        Some("Bearer wrong"),
        Some("Basic private-admin-token"),
    ] {
        let (status, headers, body) = request(state(true), "/state", auth, None).await;
        assert_eq!(status, 401);
        assert_eq!(headers["www-authenticate"], "Bearer");
        assert_eq!(headers["cache-control"], "no-store");
        assert!(body.get("facts").is_none());
        assert!(!body.to_string().contains("private-admin-token"));
    }
    let s = state(true);
    let (status, headers, body) = request(
        s.clone(),
        "/state",
        Some("Bearer private-admin-token"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(headers["content-type"], "application/json");
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(body["mqtt"]["client_id"], "private-client-id");
    assert_eq!(body["states"].as_object().unwrap().len(), 20);
    assert!(!body.to_string().contains("private-admin-token"));
    assert_eq!(body["version"], s.build.version);
}
