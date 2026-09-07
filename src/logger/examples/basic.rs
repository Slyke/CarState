use serde_json::json;
use styleguide_logger::{ErrorOptions, LogOptions, Logger};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let logger = Logger::from_files(
        directory.join("logging.example.json5"),
        directory.join("errors.json5"),
    )?;
    let outcome = logger
        .generate_log(LogOptions {
            caller: "example::main".into(),
            logger_key: Some("EXAMPLE_STARTED".into()),
            message: "Logger is ready".into(),
            correlation_id: Some("example-1".into()),
            context: Some(json!({"service": "example"})),
            ..Default::default()
        })
        .await;
    for failure in outcome.failures {
        eprintln!("{}: {}", failure.sink, failure.message);
    }
    let error = logger
        .wrap_error(ErrorOptions {
            caller: "example::read".into(),
            reason: "Example operation failed".into(),
            error_key: "EXAMPLE_READ_FAILED".into(),
            source: Some(
                std::io::Error::new(std::io::ErrorKind::NotFound, "Example input is missing")
                    .into(),
            ),
            ..Default::default()
        })
        .await;
    assert!(std::error::Error::source(&error).is_some());
    Ok(())
}
