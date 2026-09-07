// Compile the actual mod-based integration path as well as the library interface.
#[allow(dead_code, unused_imports)]
#[path = "../mod.rs"]
mod logger;

#[tokio::test]
async fn can_be_used_as_a_source_module() {
    let mut config = logger::LoggingConfig::default();
    config.sinks.console.enabled = false;
    let logger = logger::Logger::new(config, logger::ErrorCodeMap::new()).unwrap();
    let outcome = logger.generate_log(logger::LogOptions::default()).await;
    assert!(outcome.failures.is_empty());
}
