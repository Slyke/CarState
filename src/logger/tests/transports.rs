use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use styleguide_logger::{
    config::{Format, Framing, Gate, LoggingConfig, SyslogProtocol},
    ErrorCodeMap, LogOptions, Logger,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, UdpSocket},
    time::timeout,
};

fn config() -> LoggingConfig {
    let mut config = LoggingConfig::default();
    config.sinks.console.enabled = false;
    config
}

fn event() -> LogOptions {
    LogOptions {
        level: "error".into(),
        caller: "transport::test".into(),
        logger_key: Some("TRANSPORT_TEST".into()),
        message: "café 🚙".into(),
        correlation_id: Some("corr-\"\\]".into()),
        ..Default::default()
    }
}

async fn read_http<S: AsyncRead + Unpin>(stream: &mut S) -> (String, Vec<u8>) {
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0];
        stream.read_exact(&mut byte).await.unwrap();
        bytes.push(byte[0]);
        assert!(bytes.len() < 65536);
        if bytes.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let headers = String::from_utf8(bytes).unwrap();
    let length: usize = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().unwrap())
        })
        .unwrap();
    let mut body = vec![0; length];
    stream.read_exact(&mut body).await.unwrap();
    (headers, body)
}

#[tokio::test]
async fn http_sends_json_headers_method_and_query_and_honors_curl_gate_alias() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config = config();
    config.sinks.http.enabled = true;
    config.sinks.http.url = format!("http://{}/logs?source=test", listener.local_addr().unwrap());
    config.sinks.http.method = "PUT".into();
    config
        .sinks
        .http
        .headers
        .insert("x-service".into(), json!("base"));
    config
        .sinks
        .http
        .optional_headers
        .insert("x-service".into(), json!("override"));
    config
        .sinks
        .http
        .optional_headers
        .insert("x-number".into(), json!(123));
    config
        .sinks
        .http
        .headers
        .insert("content-length".into(), json!(999));
    config.gates.insert(
        "alias".into(),
        Gate {
            level: Some("error".into()),
            curl: Some(true),
            ..Default::default()
        },
    );
    let server = tokio::spawn(async move {
        timeout(Duration::from_secs(5), async {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_http(&mut stream).await;
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            request
        })
        .await
        .unwrap()
    });
    let logger = Logger::new(config, ErrorCodeMap::new()).unwrap();
    let outcome = logger
        .generate_log(LogOptions {
            level: "info".into(),
            gate: Some("alias".into()),
            ..event()
        })
        .await;
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    let (headers, body) = server.await.unwrap();
    assert!(headers.starts_with("PUT /logs?source=test HTTP/1.1\r\n"));
    assert!(headers.contains("x-service: override\r\n"));
    assert!(headers.contains("x-number: 123\r\n"));
    assert!(headers.contains("content-type: application/json\r\n"));
    let entry: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(entry["message"], "café 🚙");
    assert_eq!(entry["level"], "error");
}

#[tokio::test]
async fn http_failure_still_delivers_to_file_and_does_not_expose_url_or_body() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("delivered.jsonl");
    let mut config = config();
    config.sinks.http.enabled = true;
    config.sinks.http.url = format!(
        "http://{}/?token=secret-token",
        listener.local_addr().unwrap()
    );
    config.sinks.file.enabled = true;
    config.sinks.file.path = path.to_str().unwrap().into();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_http(&mut stream).await;
        stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 12\r\nConnection: close\r\n\r\nsecret-token").await.unwrap();
    });
    let outcome = Logger::new(config, ErrorCodeMap::new())
        .unwrap()
        .generate_log(event())
        .await;
    server.await.unwrap();
    assert_eq!(outcome.failures.len(), 1);
    assert_eq!(outcome.failures[0].sink, "http");
    assert!(outcome.failures[0].message.contains("503"));
    assert!(!format!("{:?}", outcome.failures).contains("secret-token"));
    assert!(std::fs::read_to_string(path)
        .unwrap()
        .contains("TRANSPORT_TEST"));
}

#[tokio::test]
async fn http_timeout_is_bounded_and_redirects_are_not_followed() {
    for response in [None, Some("HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/secret\r\nContent-Length: 0\r\n\r\n")] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut config = config();
        config.sinks.http.enabled = true;
        config.sinks.http.url = format!("http://{}", listener.local_addr().unwrap());
        config.sinks.http.timeout_ms = 100;
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_http(&mut stream).await;
            if let Some(response) = response { stream.write_all(response.as_bytes()).await.unwrap(); }
            std::future::pending::<()>().await;
        });
        let outcome = timeout(Duration::from_secs(3), Logger::new(config, ErrorCodeMap::new()).unwrap().generate_log(event())).await.unwrap();
        server.abort();
        assert_eq!(outcome.failures.len(), 1);
        assert!(outcome.failures[0].message.contains(if response.is_some() { "302" } else { "timed out" }));
    }
}

#[tokio::test]
async fn udp_syslog_preserves_priority_metadata_and_json_payload() {
    let receiver = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut config = config();
    config.sinks.syslog.enabled = true;
    config.sinks.syslog.host = "127.0.0.1".into();
    config.sinks.syslog.port = Some(receiver.local_addr().unwrap().port());
    config.sinks.syslog.facility = json!("local4");
    config.sinks.syslog.hostname = "host name".into();
    config.sinks.syslog.app_name = "app".into();
    config.sinks.syslog.proc_id = "proc".into();
    config.sinks.syslog.msg_id = "msg".into();
    let outcome = Logger::new(config, ErrorCodeMap::new())
        .unwrap()
        .generate_log(event())
        .await;
    assert!(outcome.failures.is_empty());
    let mut buffer = vec![0; 8192];
    let (length, _) = timeout(Duration::from_secs(3), receiver.recv_from(&mut buffer))
        .await
        .unwrap()
        .unwrap();
    let message = String::from_utf8(buffer[..length].to_vec()).unwrap();
    assert!(message.starts_with("<163>1 "));
    assert!(message.contains(" host_name app proc msg [log "));
    assert!(message.contains("correlationId=\"corr-\\\"\\\\\\]\""));
    let entry: Value = serde_json::from_str(&message[message.find('{').unwrap()..]).unwrap();
    assert_eq!(entry["message"], "café 🚙");
}

#[tokio::test]
async fn tcp_syslog_uses_utf8_byte_lengths_and_safe_newline_framing() {
    for framing in [Framing::OctetCounted, Framing::Newline] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut config = config();
        config.sinks.syslog.enabled = true;
        config.sinks.syslog.host = "127.0.0.1".into();
        config.sinks.syslog.port = Some(listener.local_addr().unwrap().port());
        config.sinks.syslog.protocol = SyslogProtocol::Tcp;
        config.sinks.syslog.framing = framing;
        config.sinks.syslog.format = Format::Text;
        let server = tokio::spawn(async move {
            timeout(Duration::from_secs(3), async {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                stream.read_to_end(&mut bytes).await.unwrap();
                String::from_utf8(bytes).unwrap()
            })
            .await
            .unwrap()
        });
        let outcome = Logger::new(config, ErrorCodeMap::new())
            .unwrap()
            .generate_log(LogOptions {
                message: "café 🚙\nsecond line".into(),
                ..event()
            })
            .await;
        assert!(outcome.failures.is_empty());
        let frame = server.await.unwrap();
        if framing == Framing::OctetCounted {
            let (count, message) = frame.split_once(' ').unwrap();
            assert_eq!(count.parse::<usize>().unwrap(), message.len());
            assert!(message.starts_with("<131>1 "));
        } else {
            assert_eq!(frame.lines().count(), 1);
            assert!(frame.contains("café 🚙\\nsecond line"));
            assert!(frame.ends_with('\n'));
        }
    }
}

fn tls_server() -> (tokio_rustls::TlsAcceptor, String) {
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let server = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![cert.der().clone()],
        rustls::pki_types::PrivatePkcs8KeyDer::from(signing_key.serialize_der()).into(),
    )
    .unwrap();
    (
        tokio_rustls::TlsAcceptor::from(Arc::new(server)),
        cert.pem(),
    )
}

#[tokio::test]
async fn syslog_tls_accepts_configured_ca_and_rejects_untrusted_certificates() {
    for trusted in [true, false] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (acceptor, ca) = tls_server();
        let mut config = config();
        config.sinks.syslog.enabled = true;
        config.sinks.syslog.protocol = SyslogProtocol::Tls;
        config.sinks.syslog.host = "127.0.0.1".into();
        config.sinks.syslog.port = Some(listener.local_addr().unwrap().port());
        config.sinks.syslog.servername = Some("localhost".into());
        config.sinks.syslog.tls_options.ca = trusted.then_some(ca);
        let server = tokio::spawn(async move {
            timeout(Duration::from_secs(5), async {
                let (stream, _) = listener.accept().await.unwrap();
                if let Ok(mut stream) = acceptor.accept(stream).await {
                    let mut buffer = Vec::new();
                    stream.read_to_end(&mut buffer).await.unwrap();
                    Some(String::from_utf8(buffer).unwrap())
                } else {
                    None
                }
            })
            .await
            .unwrap()
        });
        let outcome = Logger::new(config, ErrorCodeMap::new())
            .unwrap()
            .generate_log(event())
            .await;
        let received = server.await.unwrap();
        if trusted {
            assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
            assert!(received.unwrap().contains("TRANSPORT_TEST"));
        } else {
            assert_eq!(outcome.failures[0].sink, "syslog");
            assert!(received.is_none());
        }
    }
}

#[tokio::test]
async fn https_supports_custom_ca_and_text_format() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (acceptor, ca) = tls_server();
    let mut config = config();
    config.sinks.http.enabled = true;
    config.sinks.http.url = format!(
        "https://localhost:{}/logs",
        listener.local_addr().unwrap().port()
    );
    config.sinks.http.tls_options.ca = Some(ca);
    config.sinks.http.format = Format::Text;
    let server = tokio::spawn(async move {
        timeout(Duration::from_secs(5), async {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = acceptor.accept(stream).await.unwrap();
            let request = read_http(&mut stream).await;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK")
                .await
                .unwrap();
            stream.shutdown().await.unwrap();
            request
        })
        .await
        .unwrap()
    });
    let outcome = Logger::new(config, ErrorCodeMap::new())
        .unwrap()
        .generate_log(event())
        .await;
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    let (headers, body) = server.await.unwrap();
    assert!(headers.contains("content-type: text/plain; charset=utf-8"));
    assert!(String::from_utf8(body)
        .unwrap()
        .contains("ERROR transport::test café 🚙"));
}
