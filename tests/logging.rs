use carstate::app_log::{AppLog, Identity};
use serde_json::{json, Value};
use std::{collections::BTreeMap, time::Duration};
use styleguide_logger::{Logger, LoggingConfig};
#[tokio::test]
async fn slow_remote_logging_does_not_block_caller_and_errors_are_enriched() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let remote = tokio::spawn(async move {
        let (_stream, _) = listener.accept().await.unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;
    });
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.jsonl");
    let raw = json!({"sinks":{"console":{"enabled":false},"file":{"enabled":true,"path":path,"levels":["info","error","debug"]},"http":{"enabled":true,"url":format!("http://127.0.0.1:{port}"),"timeoutMs":150,"levels":["error"]}},"gates":{"OUTPUT_BLINK_PHASE":{"enabled":false}},"kubernetes":{"enabled":true}});
    let env = BTreeMap::from([
        ("K8S_POD_NAME".into(), "pod-one".into()),
        ("K8S_NAMESPACE".into(), "test".into()),
        ("INSTANCE_ID".into(), "instance-one".into()),
    ]);
    let settings =
        LoggingConfig::from_json5_with_environment(&raw.to_string(), &|k| env.get(k).cloned())
            .unwrap();
    let logger = Logger::new(settings, BTreeMap::new()).unwrap();
    let (log, worker) = AppLog::start(logger, Identity::new("client", &env));
    let start = std::time::Instant::now();
    log.emit(
        "error",
        "OUTPUT_SUBMISSION_FAILED",
        "sanitized failure",
        json!({"output":"test"}),
    );
    log.emit("debug", "OUTPUT_BLINK_PHASE", "phase", json!({}));
    log.emit("info", "ORDINARY_EVENT", "ordinary", json!({}));
    assert!(start.elapsed() < Duration::from_millis(100));
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!worker.is_finished());
    drop(log);
    tokio::time::timeout(Duration::from_secs(1), worker)
        .await
        .unwrap()
        .unwrap();
    remote.abort();
    let lines = std::fs::read_to_string(path).unwrap();
    let events: Vec<Value> = lines
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events.len(), 2);
    assert!(events[0].get("error").is_some());
    for e in events {
        assert_eq!(e["context"]["instanceId"], "instance-one");
        assert_eq!(e["kubernetes"]["podName"], "pod-one");
        assert!(e["context"].get("kubernetes").is_none());
    }
}
#[test]
fn validation_checks_tls_and_headers_without_remote_clients() {
    for settings in [
        json!({"sinks":{"http":{"enabled":true,"url":"http://127.0.0.1:9","headers":{"bad\nname":"hidden"}}}}),
        json!({"sinks":{"http":{"enabled":true,"url":"https://127.0.0.1:9","tlsOptions":{"ca":"hidden-invalid-pem"}}}}),
    ] {
        let parsed =
            LoggingConfig::from_json5_with_environment(&settings.to_string(), &|_| None).unwrap();
        let result = styleguide_logger::validate_transport_settings(&parsed);
        assert!(result.is_err());
        assert!(!result.unwrap_err().to_string().contains("hidden"));
    }
}
