use serde_json::{json, Value};
use std::{error::Error, io};
use styleguide_logger::{
    config::{Format, Gate, LoggingConfig},
    error_codes::ErrorCodeMap,
    format_text, ErrorOptions, LogOptions, Logger,
};

fn file_config(path: &std::path::Path) -> LoggingConfig {
    let mut config = LoggingConfig::default();
    config.sinks.console.enabled = false;
    config.sinks.file.enabled = true;
    config.sinks.file.path = path.to_str().unwrap().into();
    config.sinks.file.levels = vec![];
    config
}

fn read_lines(path: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[tokio::test]
async fn gates_change_level_before_filtering_and_never_serialize_gate_settings() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("nested/output.jsonl");
    let mut config = file_config(&path);
    config.sinks.file.levels = vec!["warn".into()];
    config.gates.insert(
        "failedLoginAttemptsExample".into(),
        Gate {
            level: Some("WARN".into()),
            ..Default::default()
        },
    );
    config.gates.insert(
        "silent".into(),
        Gate {
            enabled: Some(false),
            ..Default::default()
        },
    );
    config.gates.insert(
        "no-file".into(),
        Gate {
            file: Some(false),
            ..Default::default()
        },
    );
    let logger = Logger::new(
        config,
        ErrorCodeMap::from([("ERR_UNKNOWN".into(), "0000000000000000".into())]),
    )
    .unwrap();
    let outcome = logger
        .generate_error(ErrorOptions {
            caller: "auth::login".into(),
            reason: "Failed login".into(),
            error_key: "AUTH_FAILED_LOGIN".into(),
            gate: Some("failedLoginAttemptsExample".into()),
            context: Some(json!({"userId": 12})),
            ..Default::default()
        })
        .await;
    assert!(outcome.failures.is_empty());
    for gate in ["silent", "no-file", "unknown"] {
        logger
            .generate_log(LogOptions {
                level: if gate == "unknown" { "info" } else { "warn" }.into(),
                logger_key: Some(gate.into()),
                message: "filtered".into(),
                ..Default::default()
            })
            .await;
    }
    let lines = read_lines(&path);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["level"], "warn");
    assert_eq!(lines[0]["loggerKey"], "AUTH_FAILED_LOGIN");
    assert_eq!(lines[0]["errorCode"], "0000000000000000");
    assert_eq!(lines[0]["gateKey"], "failedLoginAttemptsExample");
    assert!(lines[0].get("gate").is_none());
    assert_eq!(lines[0]["error"]["context"]["userId"], 12);
}

#[tokio::test]
async fn repeated_wrapping_keeps_the_serialized_chain_bounded() {
    let mut config = LoggingConfig::default();
    config.sinks.console.enabled = false;
    let logger = Logger::new(config, ErrorCodeMap::new()).unwrap();
    let mut source: styleguide_logger::BoxError = io::Error::other("root cause").into();
    for i in 0..40 {
        let wrapped = logger
            .wrap_error(ErrorOptions {
                reason: format!("layer {i}"),
                source: Some(source),
                ..Default::default()
            })
            .await;
        let json = serde_json::to_string(&wrapped.details).unwrap();
        assert!(json.len() < 20000);
        if i == 39 {
            assert!(json.contains("[Error chain truncated]"));
        }
        source = Box::new(wrapped);
    }
}

#[tokio::test]
async fn explicit_gate_beats_logger_key_and_error_key_is_a_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.jsonl");
    let mut config = file_config(&path);
    config.gates.insert(
        "blocked".into(),
        Gate {
            enabled: Some(false),
            ..Default::default()
        },
    );
    let logger = Logger::new(config, ErrorCodeMap::new()).unwrap();
    logger
        .generate_log(LogOptions {
            logger_key: Some("blocked".into()),
            gate: Some("explicit".into()),
            ..Default::default()
        })
        .await;
    let details = logger
        .generate_error(ErrorOptions {
            error_key: "blocked".into(),
            ..Default::default()
        })
        .await
        .details;
    logger
        .generate_log(LogOptions {
            error: Some(details),
            ..Default::default()
        })
        .await;
    assert_eq!(read_lines(&path).len(), 1);
}

#[tokio::test]
async fn wrapped_errors_preserve_the_original_source_and_structured_chain() {
    let mut config = LoggingConfig::default();
    config.sinks.console.enabled = false;
    let logger = Logger::new(config, ErrorCodeMap::new()).unwrap();
    let inner = logger
        .wrap_error(ErrorOptions {
            reason: "Read failed".into(),
            error_key: "READ_FAILED".into(),
            source: Some(io::Error::new(io::ErrorKind::NotFound, "missing input").into()),
            ..Default::default()
        })
        .await;
    assert_eq!(
        inner
            .source()
            .unwrap()
            .downcast_ref::<io::Error>()
            .unwrap()
            .kind(),
        io::ErrorKind::NotFound
    );
    assert!(inner.details.error_code.is_none());
    assert!(inner.details.stack.is_none());
    let outer = logger
        .wrap_error(ErrorOptions {
            reason: "Request failed".into(),
            error_key: "REQUEST_FAILED".into(),
            source: Some(Box::new(inner)),
            include_stack_trace: true,
            correlation_id: Some("corr-1".into()),
            ..Default::default()
        })
        .await;
    assert_eq!(outer.to_string(), "Request failed");
    assert_eq!(
        outer.source().unwrap().source().unwrap().to_string(),
        "missing input"
    );
    let serialized = serde_json::to_value(&outer.details).unwrap();
    assert_eq!(serialized["cause"]["name"], "StructuredError");
    assert_eq!(serialized["cause"]["details"]["errorKey"], "READ_FAILED");
    assert_eq!(serialized["cause"]["cause"]["message"], "missing input");
    assert!(!outer.details.stack.unwrap().is_empty());
}

#[tokio::test]
async fn file_failures_do_not_replace_the_application_error() {
    let directory = tempfile::tempdir().unwrap();
    let logger = Logger::new(file_config(directory.path()), ErrorCodeMap::new()).unwrap();
    let error = logger
        .wrap_error(ErrorOptions {
            reason: "original error".into(),
            ..Default::default()
        })
        .await;
    assert_eq!(error.to_string(), "original error");
    assert_eq!(error.logging_failures.len(), 1);
    assert_eq!(error.logging_failures[0].sink, "file");
}

#[tokio::test]
async fn cloned_loggers_append_complete_json_records_under_concurrency() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("append.jsonl");
    let logger = Logger::new(file_config(&path), ErrorCodeMap::new()).unwrap();
    let mut tasks = Vec::new();
    for i in 0..30 {
        let logger = logger.clone();
        tasks.push(tokio::spawn(async move {
            logger
                .generate_log(LogOptions {
                    context: Some(json!({"index":i})),
                    message: "x".repeat(8192),
                    ..Default::default()
                })
                .await
        }));
    }
    for task in tasks {
        assert!(task.await.unwrap().failures.is_empty());
    }
    let lines = read_lines(&path);
    assert_eq!(lines.len(), 30);
    let indices: std::collections::BTreeSet<_> = lines
        .iter()
        .map(|line| line["context"]["index"].as_u64().unwrap())
        .collect();
    assert_eq!(indices.len(), 30);
}

#[tokio::test]
async fn text_templates_are_single_pass_and_metadata_is_optional() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("text.log");
    let mut config = file_config(&path);
    config.sinks.file.format = Format::Text;
    config.kubernetes.enabled = true;
    config.kubernetes.pod_name = Some("pod-1".into());
    config.kubernetes.namespace = Some(String::new());
    let logger = Logger::new(config, ErrorCodeMap::new()).unwrap();
    let outcome = logger
        .generate_log(LogOptions {
            message: "literal {$caller}".into(),
            caller: "demo".into(),
            ..Default::default()
        })
        .await;
    let text = format_text("{$level}|{$message}|{$caller}|{$missing}", &outcome.entry);
    assert_eq!(
        text,
        "INFO|literal {$caller}|demo| kubernetes={\"podName\":\"pod-1\"}"
    );
    assert_eq!(outcome.entry.kubernetes, Some(json!({"podName":"pod-1"})));
    assert!(std::fs::read_to_string(path)
        .unwrap()
        .contains("literal {$caller}"));
}
