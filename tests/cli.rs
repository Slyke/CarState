mod common;
use common::*;
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader},
    process::{Command, Stdio},
    time::Duration,
};
fn command(v: Value, secrets: &str) -> (tempfile::TempDir, Command) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();
    std::fs::write(path.join("config.json5"), v.to_string()).unwrap();
    std::fs::write(path.join("secrets.json5"), secrets).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carstate"));
    cmd.env_clear()
        .env("CARSTATE_CONFIG_PATH", path.join("config.json5"))
        .env("CARSTATE_SECRETS_PATH", path.join("secrets.json5"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    (dir, cmd)
}
#[test]
fn validate_only_has_no_network_listeners_or_file_mutations_and_modes_conflict() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut v = source();
    v["mqtt_settings"]["port"] = json!(listener.local_addr().unwrap().port());
    v["logging"] = json!({"sinks":{"file":{"enabled":true,"path":"/does/not/exist/carstate-test/log.jsonl"},"http":{"enabled":true,"url":format!("http://127.0.0.1:{}",listener.local_addr().unwrap().port())}}});
    let (dir, mut cmd) = command(v, "{}");
    let before = std::fs::read(dir.path().join("secrets.json5")).unwrap();
    let result = cmd.arg("--validate-config").output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(listener.accept().is_err());
    assert_eq!(
        std::fs::read(dir.path().join("secrets.json5")).unwrap(),
        before
    );
    assert!(!std::path::Path::new("/does/not/exist/carstate-test/log.jsonl").exists());
    let (_dir, mut cmd) = command(source(), "{}");
    assert!(!cmd
        .args(["--validate-config", "--dry-run"])
        .status()
        .unwrap()
        .success());
}
#[test]
fn malformed_secret_errors_and_missing_token_are_sanitized() {
    for secrets in [
        "{mqtt_password: 'super-secret', broken:}",
        "{mqtt_password: 17, http_state_token:'super-secret'}",
    ] {
        let (_dir, mut cmd) = command(source(), secrets);
        let output = cmd.arg("--validate-config").output().unwrap();
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("super-secret"));
    }
    let mut v = source();
    v["http"] = json!({"use_http":true,"state_endpoint_enabled":true});
    let (_dir, mut cmd) = command(v, "{}");
    assert!(!cmd
        .arg("--validate-config")
        .output()
        .unwrap()
        .status
        .success());
}
#[test]
fn nonfinite_optional_settings_are_rejected_instead_of_becoming_absent() {
    let (_dir, mut cmd) = command(source(), "{}");
    let file = cmd
        .get_envs()
        .find(|(k, _)| *k == "CARSTATE_CONFIG_PATH")
        .unwrap()
        .1
        .unwrap();
    let text = std::fs::read_to_string(file).unwrap().replace(
        "\"battery_low_percent\":20.0",
        "\"battery_low_percent\":NaN",
    );
    assert!(text.contains("NaN"));
    std::fs::write(file, text).unwrap();
    assert!(!cmd
        .arg("--validate-config")
        .output()
        .unwrap()
        .status
        .success());
}
#[cfg(unix)]
#[test]
fn startup_and_ordinary_errors_have_instance_and_kubernetes_metadata_http_on_and_off() {
    for http in [false, true] {
        let mut v = source();
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        v["http"] = json!({"use_http":http,"interface":"127.0.0.1","port":port});
        v["mqtt_settings"]["port"] = json!(1);
        v["logging"] = json!({"sinks":{"console":{"enabled":true,"format":"json","levels":["info","warn","error"]}}});
        let (_dir, mut cmd) = command(
            v,
            "{mqtt_username:'hidden-user',mqtt_password:'hidden-password'}",
        );
        cmd.env("INSTANCE_ID", "integration-instance")
            .env("K8S_POD_NAME", "test-pod")
            .env("K8S_NAMESPACE", "test-namespace");
        let mut child = cmd.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let boot: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(boot["loggerKey"], "SERVICE_BOOT_DIAGNOSTICS");
        assert_eq!(boot["context"]["httpEnabled"], http);
        assert_eq!(boot["context"]["instanceId"], "integration-instance");
        assert_eq!(boot["kubernetes"]["podName"], "test-pod");
        assert!(boot["context"].get("kubernetes").is_none());
        assert!(!line.contains("hidden-user"));
        assert!(!line.contains("hidden-password"));
        std::thread::sleep(Duration::from_millis(150));
        Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status()
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        let errors = String::from_utf8_lossy(&output.stderr);
        let events: Vec<Value> = errors
            .lines()
            .filter_map(|s| serde_json::from_str(s).ok())
            .collect();
        let generated: Vec<_> = events
            .iter()
            .filter(|v| v["loggerKey"] == "MQTT_CLIENT_ID_GENERATED")
            .collect();
        assert_eq!(generated.len(), 1);
        for e in events {
            assert_eq!(e["context"]["instanceId"], "integration-instance");
            assert_eq!(e["kubernetes"]["namespace"], "test-namespace");
        }
    }
}
