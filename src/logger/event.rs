use serde::Serialize;
use serde_json::Value;
use std::{error::Error, fmt};

pub type BoxError = Box<dyn Error + Send + Sync + 'static>;

#[derive(Debug, Clone)]
pub struct LogOptions {
    pub level: String,
    pub caller: String,
    pub logger_key: Option<String>,
    pub gate: Option<String>,
    pub message: String,
    pub correlation_id: Option<String>,
    pub context: Option<Value>,
    pub error: Option<ErrorDetails>,
}

impl Default for LogOptions {
    fn default() -> Self {
        Self {
            level: "info".into(),
            caller: "unknown".into(),
            logger_key: None,
            gate: None,
            message: String::new(),
            correlation_id: None,
            context: None,
            error: None,
        }
    }
}

#[derive(Debug)]
pub struct ErrorOptions {
    pub caller: String,
    pub reason: String,
    pub error_key: String,
    pub gate: Option<String>,
    pub source: Option<BoxError>,
    /// Captures a Rust backtrace at the wrapping site; original frames are unavailable.
    pub include_stack_trace: bool,
    pub correlation_id: Option<String>,
    pub context: Option<Value>,
}

impl Default for ErrorOptions {
    fn default() -> Self {
        Self {
            caller: "unknown".into(),
            reason: "Unexpected error".into(),
            error_key: "ERR_UNKNOWN".into(),
            gate: None,
            source: None,
            include_stack_trace: false,
            correlation_id: None,
            context: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorCause {
    pub name: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cause: Option<Box<ErrorCause>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Box<ErrorDetails>>,
}

impl ErrorCause {
    pub(crate) fn from_error(error: &(dyn Error + 'static), depth: usize) -> Self {
        let structured = error.downcast_ref::<StructuredError>();
        Self {
            name: if structured.is_some() {
                "StructuredError"
            } else {
                "Error"
            }
            .into(),
            message: if depth >= 32 {
                "[Error chain truncated]".into()
            } else {
                error.to_string()
            },
            cause: if depth >= 32 {
                None
            } else {
                error
                    .source()
                    .map(|e| Box::new(Self::from_error(e, depth + 1)))
            },
            details: if depth >= 32 {
                None
            } else {
                // The source chain is already represented by `cause`. Copy only the
                // details at this level to avoid exponential duplication on each wrap.
                structured.map(|e| {
                    Box::new(ErrorDetails {
                        caller: e.details.caller.clone(),
                        reason: e.details.reason.clone(),
                        error_key: e.details.error_key.clone(),
                        error_code: e.details.error_code.clone(),
                        correlation_id: e.details.correlation_id.clone(),
                        context: e.details.context.clone(),
                        cause: None,
                        stack: e.details.stack.clone(),
                    })
                })
            },
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorDetails {
    pub caller: String,
    pub reason: String,
    pub error_key: String,
    pub error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cause: Option<ErrorCause>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub timestamp: String,
    pub level: String,
    pub caller: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logger_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorDetails>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kubernetes: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SinkFailure {
    pub sink: &'static str,
    /// Safe diagnostic: never contains HTTP URLs, headers, or response bodies.
    pub message: String,
}

#[derive(Debug)]
pub struct LogOutcome {
    pub entry: LogEntry,
    pub failures: Vec<SinkFailure>,
}

#[derive(Debug)]
pub struct ErrorOutcome {
    pub details: ErrorDetails,
    pub failures: Vec<SinkFailure>,
}

#[derive(Debug)]
pub struct StructuredError {
    pub details: ErrorDetails,
    pub logging_failures: Vec<SinkFailure>,
    pub(crate) source: Option<BoxError>,
}

impl fmt::Display for StructuredError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.details.reason)
    }
}

impl Error for StructuredError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_ref()
            .map(|error| error.as_ref() as &(dyn Error + 'static))
    }
}

/// Expands only tokens in the template, never token-like text in an event value.
pub fn format_text(template: &str, entry: &LogEntry) -> String {
    let upper_level = entry.level.to_uppercase();
    let mut output = String::new();
    let mut remaining = template;
    while let Some(start) = remaining.find("{$") {
        output.push_str(&remaining[..start]);
        let token = &remaining[start + 2..];
        let Some(end) = token.find('}') else {
            output.push_str(&remaining[start..]);
            remaining = "";
            break;
        };
        output.push_str(match &token[..end] {
            "timestamp" => &entry.timestamp,
            "level" => &upper_level,
            "caller" => &entry.caller,
            "message" => &entry.message,
            "correlationId" => entry.correlation_id.as_deref().unwrap_or(""),
            "errorCode" => entry.error_code.as_deref().unwrap_or(""),
            "errorKey" => entry.error_key.as_deref().unwrap_or(""),
            "loggerKey" => entry.logger_key.as_deref().unwrap_or(""),
            _ => "",
        });
        remaining = &token[end + 1..];
    }
    output.push_str(remaining);
    if let Some(context) = &entry.context {
        output.push_str(&format!(" context={context}"));
    }
    if let Some(error) = &entry.error {
        // ErrorDetails contains only JSON-serializable owned values.
        if let Ok(json) = serde_json::to_string(error) {
            output.push_str(&format!(" error={json}"));
        }
    }
    if let Some(metadata) = &entry.kubernetes {
        output.push_str(&format!(" kubernetes={metadata}"));
    }
    output.trim().to_owned()
}
