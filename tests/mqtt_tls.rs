//! TLS checks use only a local server; no configured broker or relay is contacted.
mod common;
use carstate::{config::Secrets, mqtt};
use common::*;
use rumqttc::{tokio_rustls::TlsAcceptor, Event, Incoming};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[tokio::test]
async fn tls_client_configuration_supports_both_certificate_modes() {
    for validate_certs in [false, true] {
        let mut c = config(source());
        c.mqtt_settings.use_tls = true;
        c.mqtt_settings.validate_certs = validate_certs;
        let (_client, _events) = mqtt::connect(&c, &Secrets::default(), "tls-test");
    }
}

#[tokio::test]
async fn self_signed_tls_requires_explicitly_disabled_certificate_verification() {
    tokio::time::timeout(Duration::from_secs(10), async {
        for protocol in [&rustls::version::TLS12, &rustls::version::TLS13] {
            for validate_certs in [false, true] {
                let rcgen::CertifiedKey { cert, signing_key } =
                    rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
                let server = rustls::ServerConfig::builder_with_provider(Arc::new(
                    rustls::crypto::ring::default_provider(),
                ))
                .with_protocol_versions(&[protocol])
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(
                    vec![cert.der().clone()],
                    rustls::pki_types::PrivatePkcs8KeyDer::from(signing_key.serialize_der()).into(),
                )
                .unwrap();
                let acceptor = TlsAcceptor::from(Arc::new(server));
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let mut c = config(source());
                c.mqtt_settings.port = listener.local_addr().unwrap().port();
                c.mqtt_settings.use_tls = true;
                c.mqtt_settings.validate_certs = validate_certs;
                let (_client, mut events) = mqtt::connect(&c, &Secrets::default(), "tls-test");
                let broker = async {
                    let (stream, _) = listener.accept().await.unwrap();
                    let tls = acceptor.accept(stream).await;
                    if validate_certs {
                        assert!(tls.is_err(), "untrusted certificate must be rejected");
                        return;
                    }
                    let mut tls = tls.unwrap();
                    assert_eq!(tls.read_u8().await.unwrap(), 0x10, "expected MQTT CONNECT");
                    let length = tls.read_u8().await.unwrap();
                    assert!(length < 128, "test CONNECT fits in one length byte");
                    let mut body = vec![0; length as usize];
                    tls.read_exact(&mut body).await.unwrap();
                    assert_eq!(&body[..6], b"\0\x04MQTT");
                    tls.write_all(&[0x20, 2, 0, 0]).await.unwrap();
                };
                let (_, event) = tokio::join!(broker, events.poll());
                if validate_certs {
                    assert!(
                        event.is_err(),
                        "verified mode must reject the test certificate"
                    );
                } else {
                    assert!(matches!(
                        event.unwrap(),
                        Event::Incoming(Incoming::ConnAck(_))
                    ));
                }
            }
        }
    })
    .await
    .expect("local MQTT TLS handshake timeout");
}
