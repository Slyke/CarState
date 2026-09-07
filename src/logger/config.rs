//! JSON5 loading, whole-value environment references, and logger configuration.

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, fmt, path::Path};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(pub(crate) String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

pub(crate) fn invalid(message: &str) -> ConfigError {
    ConfigError(message.to_owned())
}

/// Resolve exact `${NAME}` string values, never keys or partial strings.
/// Missing variables become null; present values remain strings, including empty ones.
pub fn resolve_environment_references(
    value: &mut Value,
    environment: &impl Fn(&str) -> Option<String>,
) {
    match value {
        Value::String(text) => {
            if let Some(name) = text.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
                let mut chars = name.chars();
                let valid = chars
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                    && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
                if valid {
                    *value = environment(name).map(Value::String).unwrap_or(Value::Null);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                resolve_environment_references(value, environment);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                resolve_environment_references(value, environment);
            }
        }
        _ => {}
    }
}

/// Reusable application-config reader. Errors deliberately omit source values.
pub fn parse_json5_with_environment<T: DeserializeOwned>(
    text: &str,
    environment: &impl Fn(&str) -> Option<String>,
) -> Result<T, ConfigError> {
    let mut value: Value = json5::from_str(text).map_err(|_| invalid("Invalid JSON5 syntax"))?;
    resolve_environment_references(&mut value, environment);
    serde_json::from_value(value).map_err(|_| {
        invalid("JSON5 value does not match the expected schema after environment expansion")
    })
}

pub fn load_json5<T: DeserializeOwned>(path: impl AsRef<Path>) -> Result<T, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|_| invalid("Cannot read JSON5 file"))?;
    parse_json5_with_environment(&text, &|name| std::env::var(name).ok())
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Text,
    #[default]
    Json,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct OutputConfig {
    pub enabled: bool,
    pub format: Format,
    pub levels: Vec<String>,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            format: Format::Json,
            levels: vec![],
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct FileConfig {
    pub enabled: bool,
    pub format: Format,
    pub levels: Vec<String>,
    pub path: String,
}

impl Default for FileConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            format: Format::Json,
            levels: vec!["warn".into(), "error".into()],
            path: "./logs/app.jsonl".into(),
        }
    }
}

// No Debug/Serialize on secret-bearing settings: diagnostics must not dump credentials.
#[derive(Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct TlsOptions {
    /// PEM CA certificates, added to the public trust roots.
    pub ca: Option<String>,
    pub ca_file: Option<String>,
    /// Optional PEM client certificate chain and private key for mutual TLS.
    pub cert: Option<String>,
    pub key: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct HttpConfig {
    pub enabled: bool,
    pub format: Format,
    pub levels: Vec<String>,
    pub url: String,
    pub method: String,
    pub timeout_ms: u64,
    pub headers: BTreeMap<String, Value>,
    pub optional_headers: BTreeMap<String, Value>,
    pub tls_options: TlsOptions,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            format: Format::Json,
            levels: vec!["error".into()],
            url: String::new(),
            method: "POST".into(),
            timeout_ms: 2500,
            headers: BTreeMap::new(),
            optional_headers: BTreeMap::new(),
            tls_options: TlsOptions::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SyslogProtocol {
    #[default]
    Udp,
    Tcp,
    Tls,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Framing {
    #[default]
    OctetCounted,
    Newline,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SocketType {
    #[default]
    Udp4,
    Udp6,
}

#[derive(Clone, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct SyslogConfig {
    pub enabled: bool,
    pub format: Format,
    pub levels: Vec<String>,
    pub protocol: SyslogProtocol,
    pub host: String,
    pub port: Option<u16>,
    pub facility: Value,
    pub app_name: String,
    pub hostname: String,
    pub proc_id: String,
    pub msg_id: String,
    pub default_severity: String,
    pub timeout_ms: u64,
    pub framing: Framing,
    pub socket_type: SocketType,
    pub servername: Option<String>,
    pub tls_options: TlsOptions,
}

impl Default for SyslogConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            format: Format::Json,
            levels: vec!["warn".into(), "error".into()],
            protocol: SyslogProtocol::Udp,
            host: "localhost".into(),
            port: None,
            facility: json!("local0"),
            app_name: "rust".into(),
            hostname: std::env::var("HOSTNAME").unwrap_or_else(|_| "-".into()),
            proc_id: std::process::id().to_string(),
            msg_id: "app-log".into(),
            default_severity: "info".into(),
            timeout_ms: 2500,
            framing: Framing::OctetCounted,
            socket_type: SocketType::Udp4,
            servername: None,
            tls_options: TlsOptions::default(),
        }
    }
}

impl SyslogConfig {
    pub fn port(&self) -> u16 {
        self.port
            .unwrap_or(if self.protocol == SyslogProtocol::Tls {
                6514
            } else {
                514
            })
    }
}

#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SinksConfig {
    /// Like JS console: warnings/errors to stderr, other levels to stdout.
    pub console: OutputConfig,
    pub stdout: OutputConfig,
    pub stderr: OutputConfig,
    pub file: FileConfig,
    pub http: HttpConfig,
    pub syslog: SyslogConfig,
}

impl Default for SinksConfig {
    fn default() -> Self {
        Self {
            console: OutputConfig {
                enabled: true,
                format: Format::Text,
                levels: vec![],
            },
            stdout: OutputConfig::default(),
            stderr: OutputConfig::default(),
            file: FileConfig::default(),
            http: HttpConfig::default(),
            syslog: SyslogConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Gate {
    pub enabled: Option<bool>,
    pub level: Option<String>,
    pub console: Option<bool>,
    pub stdout: Option<bool>,
    pub stderr: Option<bool>,
    pub file: Option<bool>,
    pub http: Option<bool>,
    pub curl: Option<bool>,
    pub syslog: Option<bool>,
}

impl Gate {
    pub(crate) fn allows(&self, sink: &str) -> bool {
        if self.enabled == Some(false) {
            return false;
        }
        match sink {
            "console" => self.console,
            "stdout" => self.stdout,
            "stderr" => self.stderr,
            "file" => self.file,
            "http" => self.http.or(self.curl),
            "syslog" => self.syslog,
            _ => None,
        }
        .unwrap_or(true)
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct KubernetesConfig {
    pub enabled: bool,
    pub pod_name: Option<String>,
    pub deployment: Option<String>,
    pub namespace: Option<String>,
    pub pod_ip: Option<String>,
    pub pod_ips: Option<Value>,
    pub node_name: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct LoggingConfig {
    pub log_text_format: String,
    pub error_file: Option<String>,
    pub sinks: SinksConfig,
    pub gates: BTreeMap<String, Gate>,
    pub kubernetes: KubernetesConfig,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            log_text_format: "[{$timestamp}] {$level} {$caller} {$message}".into(),
            error_file: None,
            sinks: SinksConfig::default(),
            gates: BTreeMap::new(),
            kubernetes: KubernetesConfig::default(),
        }
    }
}

impl LoggingConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path)
            .map_err(|_| invalid("Cannot read logging config file"))?;
        Self::from_json5(&text)
    }

    pub fn from_json5(text: &str) -> Result<Self, ConfigError> {
        Self::from_json5_with_environment(text, &|name| std::env::var(name).ok())
    }

    /// Accepts either `{ logging: { ... } }` or a logging object directly.
    /// Precedence: defaults < legacy LOG_* environment variables < JSON5 settings.
    pub fn from_json5_with_environment(
        text: &str,
        environment: &impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ConfigError> {
        let mut settings: Value =
            json5::from_str(text).map_err(|_| invalid("Invalid logging JSON5 syntax"))?;
        resolve_environment_references(&mut settings, environment);
        let settings = settings.get("logging").cloned().unwrap_or(settings);
        if !settings.is_object() {
            return Err(invalid("Logging settings must be an object"));
        }
        let mut base = legacy_environment(environment)?;
        merge(&mut base, settings);
        normalize_headers(&mut base)?;
        let mut config: Self = serde_json::from_value(base).map_err(|_| {
            invalid(
                "Invalid logging settings: check field names and types after environment expansion",
            )
        })?;
        config.normalize();
        config.validate()?;
        Ok(config)
    }

    pub(crate) fn normalize(&mut self) {
        for levels in [
            &mut self.sinks.console.levels,
            &mut self.sinks.stdout.levels,
            &mut self.sinks.stderr.levels,
            &mut self.sinks.file.levels,
            &mut self.sinks.http.levels,
            &mut self.sinks.syslog.levels,
        ] {
            *levels = levels
                .iter()
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty())
                .collect();
        }
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.sinks.file.enabled && self.sinks.file.path.is_empty() {
            return Err(invalid("Enabled file sink requires a path"));
        }
        let http = &self.sinks.http;
        if http.enabled {
            let url =
                reqwest::Url::parse(&http.url).map_err(|_| invalid("Invalid HTTP sink URL"))?;
            if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
                return Err(invalid(
                    "HTTP sink URL must use http or https and include a host",
                ));
            }
            if !url.username().is_empty() || url.password().is_some() {
                return Err(invalid(
                    "Use HTTP headers for authentication instead of URL credentials",
                ));
            }
            reqwest::Method::from_bytes(http.method.as_bytes())
                .map_err(|_| invalid("Invalid HTTP sink method"))?;
            if http.timeout_ms == 0 {
                return Err(invalid("HTTP sink timeoutMs must be positive"));
            }
        }
        let syslog = &self.sinks.syslog;
        if syslog.enabled {
            if syslog.host.is_empty() || syslog.port() == 0 || syslog.timeout_ms == 0 {
                return Err(invalid(
                    "Syslog requires a host, nonzero port, and positive timeoutMs",
                ));
            }
            if facility_code(&syslog.facility).is_none() {
                return Err(invalid("Invalid syslog facility"));
            }
            if severity_code(&syslog.default_severity).is_none() {
                return Err(invalid("Invalid syslog defaultSeverity"));
            }
        }
        Ok(())
    }
}

fn merge(base: &mut Value, settings: Value) {
    match (base, settings) {
        (Value::Object(base), Value::Object(settings)) => {
            for (key, value) in settings {
                merge(base.entry(key).or_insert(Value::Null), value);
            }
        }
        (base, value) => *base = value,
    }
}

fn normalize_headers(value: &mut Value) -> Result<(), ConfigError> {
    for field in ["headers", "optionalHeaders"] {
        if let Some(headers) = value.pointer_mut(&format!("/sinks/http/{field}")) {
            let headers = headers
                .as_object_mut()
                .ok_or_else(|| invalid("HTTP headers must be an object"))?;
            headers.retain(|_, value| !value.is_null());
            for value in headers.values_mut() {
                match value {
                    Value::String(_) => {}
                    Value::Bool(_) | Value::Number(_) => *value = Value::String(value.to_string()),
                    _ => {
                        return Err(invalid(
                            "HTTP header values must be strings, numbers, booleans, or null",
                        ))
                    }
                }
            }
        }
    }
    Ok(())
}

fn legacy_environment(environment: &impl Fn(&str) -> Option<String>) -> Result<Value, ConfigError> {
    let mut value = json!({"sinks": {}, "kubernetes": {}});
    for (variable, field) in [
        ("LOG_TEXT_FORMAT", "logTextFormat"),
        ("ERROR_FILE_PATH", "errorFile"),
    ] {
        if let Some(text) = environment(variable) {
            value[field] = json!(text);
        }
    }
    // Compatibility aliases from logger.js; new settings should use JSON5 references.
    for sink in ["console", "stdout", "stderr", "file", "http", "syslog"] {
        let mut settings = if sink == "console" {
            json!({"enabled": true, "format": "text"})
        } else {
            json!({})
        };
        for (suffix, field, kind) in [
            ("ENABLED", "enabled", "bool"),
            ("FORMAT", "format", "string"),
            ("LEVELS", "levels", "levels"),
            ("PATH", "path", "string"),
            ("URL", "url", "string"),
            ("METHOD", "method", "string"),
            ("TIMEOUT_MS", "timeoutMs", "number"),
            ("HEADERS", "headers", "json"),
            ("PROTOCOL", "protocol", "string"),
            ("HOST", "host", "string"),
            ("PORT", "port", "number"),
            ("FACILITY", "facility", "string"),
            ("APP_NAME", "appName", "string"),
            ("HOSTNAME", "hostname", "string"),
            ("PROC_ID", "procId", "string"),
            ("MSG_ID", "msgId", "string"),
            ("DEFAULT_SEVERITY", "defaultSeverity", "string"),
            ("FRAMING", "framing", "string"),
            ("SOCKET_TYPE", "socketType", "string"),
            ("SERVERNAME", "servername", "string"),
        ] {
            let applicable = matches!(field, "enabled" | "format" | "levels")
                || (sink == "file" && field == "path")
                || (sink == "http" && matches!(field, "url" | "method" | "timeoutMs" | "headers"))
                || (sink == "syslog" && !matches!(field, "path" | "url" | "method" | "headers"));
            if !applicable {
                continue;
            }
            let variable = format!("LOG_{}_{suffix}", sink.to_uppercase());
            if let Some(text) = environment(&variable) {
                let bad = || ConfigError(format!("Invalid value for {variable}"));
                settings[field] = match kind {
                    "bool" => json!(text.to_lowercase().parse::<bool>().map_err(|_| bad())?),
                    "number" => json!(text.parse::<u64>().map_err(|_| bad())?),
                    "levels" => json!(text
                        .split(',')
                        .map(|s| s.trim().to_lowercase())
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()),
                    "json" => json5::from_str::<Value>(&text).map_err(|_| bad())?,
                    _ => json!(text),
                };
            }
        }
        value["sinks"][sink] = settings;
    }
    if let Some(text) = environment("LOG_K8S_METADATA_ENABLED") {
        value["kubernetes"]["enabled"] = json!(text
            .to_lowercase()
            .parse::<bool>()
            .map_err(|_| invalid("Invalid LOG_K8S_METADATA_ENABLED"))?);
    }
    for (variable, field) in [
        ("K8S_POD_NAME", "podName"),
        ("K8S_DEPLOYMENT", "deployment"),
        ("K8S_NAMESPACE", "namespace"),
        ("K8S_POD_IP", "podIp"),
        ("K8S_POD_IPS", "podIps"),
        ("K8S_NODE_NAME", "nodeName"),
    ] {
        if let Some(text) = environment(variable) {
            value["kubernetes"][field] = json!(text);
        }
    }
    Ok(value)
}

pub(crate) fn facility_code(facility: &Value) -> Option<u8> {
    if let Some(number) = facility
        .as_u64()
        .or_else(|| facility.as_str()?.parse().ok())
    {
        return (number < 24).then_some(number as u8);
    }
    Some(match facility.as_str()?.to_lowercase().as_str() {
        "kern" => 0,
        "user" => 1,
        "mail" => 2,
        "daemon" => 3,
        "auth" => 4,
        "syslog" => 5,
        "lpr" => 6,
        "news" => 7,
        "uucp" => 8,
        "cron" => 9,
        "authpriv" => 10,
        "ftp" => 11,
        "ntp" => 12,
        "audit" | "security" => 13,
        "alert" | "console" => 14,
        "clock" | "solariscron" => 15,
        "local0" => 16,
        "local1" => 17,
        "local2" => 18,
        "local3" => 19,
        "local4" => 20,
        "local5" => 21,
        "local6" => 22,
        "local7" => 23,
        _ => return None,
    })
}

pub(crate) fn severity_code(level: &str) -> Option<u8> {
    Some(match level.to_lowercase().as_str() {
        "emerg" | "emergency" | "panic" => 0,
        "alert" => 1,
        "crit" | "critical" | "fatal" => 2,
        "err" | "error" => 3,
        "warn" | "warning" => 4,
        "notice" => 5,
        "info" | "informational" => 6,
        "debug" | "trace" => 7,
        _ => return None,
    })
}
