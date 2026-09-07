//! Portable async structured logger and error-code tools.
//!
//! Use as a path dependency (`styleguide-logger = { path = "src/logger" }`),
//! or as `#[path = "logger/mod.rs"] mod logger;` with the manifest's dependencies.
//! Calls must be awaited inside a Tokio runtime. All selected sinks finish (or time
//! out) before return; there are no detached workers or shutdown queues to drain.

pub mod config;
pub mod error_codes;
mod event;
mod sinks;

pub use self::config::{ConfigError, LoggingConfig};
pub use self::error_codes::ErrorCodeMap;
pub use self::event::*;
pub use self::sinks::format_syslog;

use chrono::{SecondsFormat, Utc};
use std::{backtrace::Backtrace, path::Path, sync::Arc};

/// Clones share their transports and file-write lock; no process-global logger is installed.
#[derive(Clone)]
pub struct Logger {
    inner: Arc<Inner>,
}

struct Inner {
    config: LoggingConfig,
    codes: ErrorCodeMap,
    sinks: sinks::Sinks,
    kubernetes: Option<serde_json::Value>,
}

impl Logger {
    pub fn new(mut config: LoggingConfig, codes: ErrorCodeMap) -> Result<Self, ConfigError> {
        config.normalize();
        config.validate()?;
        error_codes::validate_error_codes(&codes)?;
        let sinks = sinks::Sinks::new(&config)?;
        let kubernetes = if config.kubernetes.enabled {
            let mut metadata = serde_json::to_value(&config.kubernetes)
                .map_err(|_| config::invalid("Invalid Kubernetes metadata"))?;
            if let Some(object) = metadata.as_object_mut() {
                object.retain(|key, value| {
                    key != "enabled" && !value.is_null() && value.as_str() != Some("")
                });
                if object.is_empty() {
                    None
                } else {
                    Some(metadata)
                }
            } else {
                None
            }
        } else {
            None
        };
        Ok(Self {
            inner: Arc::new(Inner {
                config,
                codes,
                sinks,
                kubernetes,
            }),
        })
    }

    /// Load JSON5 settings and the optional configured errorFile/ERROR_FILE_PATH.
    /// Relative paths are interpreted against the current working directory.
    pub fn from_config_file(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let config = LoggingConfig::load(path)?;
        let codes = match &config.error_file {
            Some(path) => error_codes::load_error_codes(path)?,
            None => ErrorCodeMap::new(),
        };
        Self::new(config, codes)
    }

    /// Both inputs accept JSON5 and recursively expand whole-value environment references.
    pub fn from_files(
        config_path: impl AsRef<Path>,
        error_path: impl AsRef<Path>,
    ) -> Result<Self, ConfigError> {
        Self::new(
            LoggingConfig::load(config_path)?,
            error_codes::load_error_codes(error_path)?,
        )
    }

    pub async fn generate_log(&self, options: LogOptions) -> LogOutcome {
        let config = &self.inner.config;
        let gate_key = options
            .gate
            .or_else(|| options.logger_key.clone())
            .or_else(|| options.error.as_ref().map(|e| e.error_key.clone()));
        let gate = gate_key.as_ref().and_then(|key| config.gates.get(key));
        let level = gate
            .and_then(|gate| gate.level.as_ref())
            .unwrap_or(&options.level)
            .to_lowercase();
        let entry = LogEntry {
            timestamp: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            level,
            caller: options.caller,
            message: options.message,
            correlation_id: options.correlation_id,
            logger_key: options.logger_key,
            gate_key,
            context: options.context,
            error_key: options.error.as_ref().map(|error| error.error_key.clone()),
            error_code: options
                .error
                .as_ref()
                .and_then(|error| error.error_code.clone()),
            error: options.error,
            kubernetes: self.inner.kubernetes.clone(),
        };
        let text = format_text(&config.log_text_format, &entry);
        let json = match serde_json::to_string(&entry) {
            Ok(json) => json,
            Err(_) => {
                return LogOutcome {
                    entry,
                    failures: vec![SinkFailure {
                        sink: "serialization",
                        message: "Cannot serialize log entry".into(),
                    }],
                }
            }
        };
        let emit = |name| {
            self.inner
                .sinks
                .emit(name, config, &entry, gate, &text, &json)
        };
        let (console, stdout, stderr, file, http, syslog) = tokio::join!(
            emit("console"),
            emit("stdout"),
            emit("stderr"),
            emit("file"),
            emit("http"),
            emit("syslog"),
        );
        LogOutcome {
            entry,
            failures: [console, stdout, stderr, file, http, syslog]
                .into_iter()
                .flatten()
                .collect(),
        }
    }

    pub async fn generate_error(&self, options: ErrorOptions) -> ErrorOutcome {
        self.generate_error_ref(&options).await
    }

    async fn generate_error_ref(&self, options: &ErrorOptions) -> ErrorOutcome {
        let details = ErrorDetails {
            caller: options.caller.clone(),
            reason: options.reason.clone(),
            error_key: options.error_key.clone(),
            error_code: self
                .inner
                .codes
                .get(&options.error_key)
                .or_else(|| self.inner.codes.get("ERR_UNKNOWN"))
                .cloned(),
            correlation_id: options.correlation_id.clone(),
            context: options.context.clone(),
            cause: options
                .source
                .as_ref()
                .map(|source| ErrorCause::from_error(source.as_ref(), 0)),
            stack: options
                .include_stack_trace
                .then(|| Backtrace::force_capture().to_string()),
        };
        let outcome = self
            .generate_log(LogOptions {
                level: "error".into(),
                caller: options.caller.clone(),
                logger_key: Some(options.error_key.clone()),
                gate: options.gate.clone(),
                message: options.reason.clone(),
                correlation_id: options.correlation_id.clone(),
                context: options.context.clone(),
                error: Some(details.clone()),
            })
            .await;
        ErrorOutcome {
            details,
            failures: outcome.failures,
        }
    }

    /// Preserves the owned original error for `std::error::Error::source` and downcasting.
    pub async fn wrap_error(&self, options: ErrorOptions) -> StructuredError {
        let outcome = self.generate_error_ref(&options).await;
        StructuredError {
            details: outcome.details,
            logging_failures: outcome.failures,
            source: options.source,
        }
    }
}

/// Validate headers, certificate files and TLS material without creating remote clients or opening sockets.
/// Applications use this for validation-only CLI modes before constructing a Logger.
pub fn validate_transport_settings(config: &LoggingConfig) -> Result<(), ConfigError> {
    config.validate()?;
    sinks::validate_transport_settings(config)
}
