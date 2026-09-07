use std::process::Command;
use styleguide_logger::{ErrorCodeMap, LogOptions, Logger, LoggingConfig};

#[tokio::test]
async fn terminal_child() {
    let Ok(mode) = std::env::var("LOGGER_TEST_TERMINAL_MODE") else {
        return;
    };
    let mut config = LoggingConfig::default();
    config.sinks.console.enabled = mode == "console";
    config.sinks.stdout.enabled = mode == "explicit";
    config.sinks.stdout.levels = vec!["info".into()];
    config.sinks.stderr.enabled = mode == "explicit";
    config.sinks.stderr.levels = vec!["error".into()];
    let logger = Logger::new(config, ErrorCodeMap::new()).unwrap();
    for (level, message) in [("info", "STDOUT_EVENT"), ("error", "STDERR_EVENT")] {
        let outcome = logger
            .generate_log(LogOptions {
                level: level.into(),
                message: message.into(),
                ..Default::default()
            })
            .await;
        assert!(outcome.failures.is_empty());
    }
}

#[test]
fn console_and_explicit_streams_route_to_the_correct_descriptors() {
    for mode in ["console", "explicit"] {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "terminal_child", "--nocapture"])
            .env("LOGGER_TEST_TERMINAL_MODE", mode)
            .output()
            .unwrap();
        assert!(output.status.success());
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stdout.contains("STDOUT_EVENT"));
        assert!(!stdout.contains("STDERR_EVENT"));
        assert!(stderr.contains("STDERR_EVENT"));
        assert!(!stderr.contains("STDOUT_EVENT"));
    }
}
