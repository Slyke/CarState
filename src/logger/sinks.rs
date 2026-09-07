use super::{
    config::{
        facility_code, invalid, severity_code, ConfigError, Format, Framing, Gate, LoggingConfig,
        SocketType, SyslogConfig, SyslogProtocol, TlsOptions,
    },
    event::{LogEntry, SinkFailure},
};
use reqwest::{
    header::{HeaderMap, HeaderName, HeaderValue, CONTENT_LENGTH, CONTENT_TYPE},
    Client, Method,
};
use std::{io, path::Path, sync::Arc, time::Duration};
use tokio::{
    io::AsyncWriteExt,
    net::{TcpStream, UdpSocket},
    sync::Mutex,
};
use tokio_rustls::TlsConnector;

pub(crate) struct Sinks {
    http: Option<(Client, Method, HeaderMap)>,
    tls: Option<TlsConnector>,
    file_lock: Mutex<()>,
}

impl Sinks {
    pub(crate) fn new(config: &LoggingConfig) -> Result<Self, ConfigError> {
        let http = &config.sinks.http;
        let http_client = if http.enabled {
            let mut headers = HeaderMap::new();
            for (key, value) in http.headers.iter().chain(http.optional_headers.iter()) {
                if value.is_null() {
                    continue;
                }
                let name = HeaderName::from_bytes(key.as_bytes())
                    .map_err(|_| invalid("Invalid HTTP header name"))?;
                let text = match value {
                    serde_json::Value::String(value) => value.clone(),
                    serde_json::Value::Bool(_) | serde_json::Value::Number(_) => value.to_string(),
                    _ => return Err(invalid("Invalid HTTP header value type")),
                };
                let mut value = HeaderValue::from_str(&text)
                    .map_err(|_| invalid("Invalid HTTP header value"))?;
                value.set_sensitive(true);
                headers.insert(name, value);
            }
            // Content length/type belong to the logger's serialized payload.
            headers.remove(CONTENT_LENGTH);
            headers.remove(CONTENT_TYPE);
            let client = Client::builder()
                .timeout(Duration::from_millis(http.timeout_ms))
                .redirect(reqwest::redirect::Policy::none())
                .use_preconfigured_tls(tls_config(&http.tls_options)?)
                .build()
                .map_err(|_| invalid("Cannot initialize HTTP logging client"))?;
            let method = Method::from_bytes(http.method.as_bytes())
                .map_err(|_| invalid("Invalid HTTP sink method"))?;
            Some((client, method, headers))
        } else {
            None
        };
        let syslog = &config.sinks.syslog;
        let tls = if syslog.enabled && syslog.protocol == SyslogProtocol::Tls {
            let name = syslog
                .servername
                .as_deref()
                .unwrap_or(&syslog.host)
                .to_owned();
            rustls::pki_types::ServerName::try_from(name)
                .map_err(|_| invalid("Invalid syslog TLS servername"))?;
            Some(TlsConnector::from(Arc::new(tls_config(
                &syslog.tls_options,
            )?)))
        } else {
            None
        };
        Ok(Self {
            http: http_client,
            tls,
            file_lock: Mutex::new(()),
        })
    }

    pub(crate) async fn emit(
        &self,
        name: &'static str,
        config: &LoggingConfig,
        entry: &LogEntry,
        gate: Option<&Gate>,
        text: &str,
        json: &str,
    ) -> Option<SinkFailure> {
        let sinks = &config.sinks;
        let (enabled, format, levels) = match name {
            "console" => (
                sinks.console.enabled,
                sinks.console.format,
                &sinks.console.levels,
            ),
            "stdout" => (
                sinks.stdout.enabled,
                sinks.stdout.format,
                &sinks.stdout.levels,
            ),
            "stderr" => (
                sinks.stderr.enabled,
                sinks.stderr.format,
                &sinks.stderr.levels,
            ),
            "file" => (sinks.file.enabled, sinks.file.format, &sinks.file.levels),
            "http" => (sinks.http.enabled, sinks.http.format, &sinks.http.levels),
            "syslog" => (
                sinks.syslog.enabled,
                sinks.syslog.format,
                &sinks.syslog.levels,
            ),
            _ => return None,
        };
        if !enabled
            || (!levels.is_empty() && !levels.contains(&entry.level))
            || gate.is_some_and(|g| !g.allows(name))
        {
            return None;
        }
        let payload = if format == Format::Text { text } else { json };
        let result = match name {
            "console" | "stdout" | "stderr" => {
                let stderr = name == "stderr"
                    || (name == "console"
                        && matches!(entry.level.as_str(), "warn" | "warning" | "error"));
                let line = format!("{payload}\n");
                if stderr {
                    let mut output = tokio::io::stderr();
                    async {
                        output.write_all(line.as_bytes()).await?;
                        output.flush().await
                    }
                    .await
                } else {
                    let mut output = tokio::io::stdout();
                    async {
                        output.write_all(line.as_bytes()).await?;
                        output.flush().await
                    }
                    .await
                }
                .map_err(|error: io::Error| format!("Output write failed ({:?})", error.kind()))
            }
            "file" => self
                .write_file(&sinks.file.path, payload)
                .await
                .map_err(|error| format!("File write failed ({:?})", error.kind())),
            "http" => self.send_http(config, payload, format).await,
            "syslog" => {
                let message = format_syslog(&sinks.syslog, entry, payload);
                match tokio::time::timeout(
                    Duration::from_millis(sinks.syslog.timeout_ms),
                    self.send_syslog(&sinks.syslog, &message),
                )
                .await
                {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(error)) => Err(format!("Syslog delivery failed ({:?})", error.kind())),
                    Err(_) => Err("Syslog delivery timed out".into()),
                }
            }
            _ => Ok(()),
        };
        result.err().map(|message| SinkFailure {
            sink: name,
            message,
        })
    }

    async fn write_file(&self, path: &str, payload: &str) -> io::Result<()> {
        let _guard = self.file_lock.lock().await;
        if let Some(parent) = Path::new(path)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
        {
            tokio::fs::create_dir_all(parent).await?;
        }
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await?;
        file.write_all(format!("{payload}\n").as_bytes()).await?;
        file.flush().await
    }

    async fn send_http(
        &self,
        config: &LoggingConfig,
        payload: &str,
        format: Format,
    ) -> Result<(), String> {
        let Some((client, method, headers)) = &self.http else {
            return Err("HTTP client unavailable".into());
        };
        let content_type = if format == Format::Text {
            "text/plain; charset=utf-8"
        } else {
            "application/json"
        };
        let mut response = client
            .request(method.clone(), &config.sinks.http.url)
            .headers(headers.clone())
            .header(CONTENT_TYPE, content_type)
            .body(payload.to_owned())
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    "HTTP delivery timed out"
                } else {
                    "HTTP delivery failed"
                }
                .to_owned()
            })?;
        if !response.status().is_success() {
            return Err(format!(
                "HTTP sink returned status {}",
                response.status().as_u16()
            ));
        }
        // Drain incrementally to permit connection reuse without buffering a response body.
        while response
            .chunk()
            .await
            .map_err(|_| "HTTP response read failed".to_owned())?
            .is_some()
        {}
        Ok(())
    }

    async fn send_syslog(&self, config: &SyslogConfig, message: &str) -> io::Result<()> {
        if config.protocol == SyslogProtocol::Udp {
            let address = tokio::net::lookup_host((config.host.as_str(), config.port()))
                .await?
                .find(|address| address.is_ipv4() == (config.socket_type == SocketType::Udp4))
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::AddrNotAvailable,
                        "No matching syslog address",
                    )
                })?;
            let bind = if address.is_ipv4() {
                "0.0.0.0:0"
            } else {
                "[::]:0"
            };
            let socket = UdpSocket::bind(bind).await?;
            socket.send_to(message.as_bytes(), address).await?;
            return Ok(());
        }
        let framed = match config.framing {
            Framing::OctetCounted => format!("{} {message}", message.len()),
            Framing::Newline => format!("{}\n", message.replace('\r', "\\r").replace('\n', "\\n")),
        };
        let mut stream = TcpStream::connect((config.host.as_str(), config.port())).await?;
        if config.protocol == SyslogProtocol::Tls {
            let connector = self
                .tls
                .as_ref()
                .ok_or_else(|| io::Error::other("TLS connector unavailable"))?;
            let name = rustls::pki_types::ServerName::try_from(
                config
                    .servername
                    .as_deref()
                    .unwrap_or(&config.host)
                    .to_owned(),
            )
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid servername"))?;
            let mut stream = connector.connect(name, stream).await?;
            stream.write_all(framed.as_bytes()).await?;
            stream.shutdown().await
        } else {
            stream.write_all(framed.as_bytes()).await?;
            stream.shutdown().await
        }
    }
}

fn tls_config(options: &TlsOptions) -> Result<rustls::ClientConfig, ConfigError> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let file_ca = options
        .ca_file
        .as_ref()
        .map(|path| std::fs::read_to_string(path).map_err(|_| invalid("Cannot read TLS CA file")))
        .transpose()?;
    for pem in options.ca.iter().chain(file_ca.iter()) {
        let certificates = rustls_pemfile::certs(&mut pem.as_bytes())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| invalid("Invalid TLS CA PEM"))?;
        if certificates.is_empty() {
            return Err(invalid("TLS CA PEM contains no certificates"));
        }
        for cert in certificates {
            roots
                .add(cert)
                .map_err(|_| invalid("Invalid TLS CA certificate"))?;
        }
    }
    let builder = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|_| invalid("Cannot configure TLS protocols"))?
    .with_root_certificates(roots);
    match (&options.cert, &options.key) {
        (None, None) => Ok(builder.with_no_client_auth()),
        (Some(cert), Some(key)) => {
            let chain = rustls_pemfile::certs(&mut cert.as_bytes())
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| invalid("Invalid TLS client certificate PEM"))?;
            let key = rustls_pemfile::private_key(&mut key.as_bytes())
                .map_err(|_| invalid("Invalid TLS client key PEM"))?
                .ok_or_else(|| invalid("TLS client key PEM contains no private key"))?;
            builder
                .with_client_auth_cert(chain, key)
                .map_err(|_| invalid("Invalid TLS client identity"))
        }
        _ => Err(invalid(
            "TLS client authentication requires both cert and key",
        )),
    }
}

/// RFC5424-shaped message, compatible with the styleguide's `[log ...]` data element.
pub fn format_syslog(config: &SyslogConfig, entry: &LogEntry, payload: &str) -> String {
    let severity = severity_code(&entry.level)
        .or_else(|| severity_code(&config.default_severity))
        .unwrap_or(6);
    let priority = facility_code(&config.facility).unwrap_or(16) * 8 + severity;
    let params: Vec<String> = [
        ("level", Some(entry.level.as_str())),
        ("caller", Some(entry.caller.as_str())),
        ("loggerKey", entry.logger_key.as_deref()),
        ("errorKey", entry.error_key.as_deref()),
        ("errorCode", entry.error_code.as_deref()),
        ("correlationId", entry.correlation_id.as_deref()),
        ("gateKey", entry.gate_key.as_deref()),
    ]
    .into_iter()
    .filter_map(|(key, value)| {
        value.filter(|s| !s.is_empty()).map(|value| {
            let escaped = value
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace(']', "\\]")
                .replace('\r', "\\r")
                .replace('\n', "\\n");
            format!("{key}=\"{escaped}\"")
        })
    })
    .collect();
    let data = if params.is_empty() {
        "-".into()
    } else {
        format!("[log {}]", params.join(" "))
    };
    format!(
        "<{priority}>1 {} {} {} {} {} {data} {payload}",
        entry.timestamp,
        header(&config.hostname, 255),
        header(&config.app_name, 48),
        header(&config.proc_id, 128),
        header(&config.msg_id, 32)
    )
}

fn header(value: &str, max_length: usize) -> String {
    if value.is_empty() {
        return "-".into();
    }
    value
        .chars()
        .map(|c| if c.is_ascii_graphic() { c } else { '_' })
        .take(max_length)
        .collect()
}

// Read-only validation shared with applications' no-network configuration checks.
pub(crate) fn validate_transport_settings(config: &LoggingConfig) -> Result<(), ConfigError> {
    let http = &config.sinks.http;
    if http.enabled {
        for (key, value) in http.headers.iter().chain(http.optional_headers.iter()) {
            if value.is_null() {
                continue;
            }
            HeaderName::from_bytes(key.as_bytes())
                .map_err(|_| invalid("Invalid HTTP header name"))?;
            let text = match value {
                serde_json::Value::String(v) => v.clone(),
                serde_json::Value::Bool(_) | serde_json::Value::Number(_) => value.to_string(),
                _ => return Err(invalid("Invalid HTTP header value type")),
            };
            HeaderValue::from_str(&text).map_err(|_| invalid("Invalid HTTP header value"))?;
        }
        tls_config(&http.tls_options)?;
    }
    let syslog = &config.sinks.syslog;
    if syslog.enabled && syslog.protocol == SyslogProtocol::Tls {
        let name = syslog
            .servername
            .as_deref()
            .unwrap_or(&syslog.host)
            .to_owned();
        rustls::pki_types::ServerName::try_from(name)
            .map_err(|_| invalid("Invalid syslog TLS servername"))?;
        tls_config(&syslog.tls_options)?;
    }
    Ok(())
}
