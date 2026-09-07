use crate::{
    config::{Config, Secrets},
    engine::{Publication, SubmissionError},
};
use rumqttc::{AsyncClient, Event, EventLoop, MqttOptions, QoS, Transport};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{sync::mpsc, time::Instant};

pub fn qos(value: u8) -> QoS {
    if value == 0 {
        QoS::AtMostOnce
    } else {
        QoS::AtLeastOnce
    }
}
pub fn connect(c: &Config, secrets: &Secrets, id: &str) -> (AsyncClient, EventLoop) {
    let mut options = MqttOptions::new(id, &c.mqtt_settings.ip, c.mqtt_settings.port);
    options
        .set_keep_alive(Duration::from_secs(c.mqtt_settings.keep_alive_seconds))
        .set_clean_session(true)
        .set_max_packet_size(1024 * 1024, 1024 * 1024);
    if !secrets.mqtt_username.is_empty() || !secrets.mqtt_password.is_empty() {
        options.set_credentials(&secrets.mqtt_username, &secrets.mqtt_password);
    }
    if c.mqtt_settings.use_tls {
        if c.mqtt_settings.validate_certs {
            options.set_transport(Transport::tls_with_default_config());
        } else {
            let tls = rustls::ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(UnverifiedCertificate))
                .with_no_client_auth();
            options.set_transport(Transport::tls_with_config(tls.into()));
        }
    }
    // No Last Will, including in dry-run. Recreate both halves on every disconnect.
    let (client, mut events) = AsyncClient::new(options, c.outputs.len() + 1);
    events
        .network_options
        .set_connection_timeout(c.mqtt_settings.publish_timeout_seconds.ceil() as u64);
    (client, events)
}
pub fn submit(client: &AsyncClient, p: &Publication) -> Result<(), SubmissionError> {
    // try_publish has definite, synchronous pre-acceptance failure; it cannot stall the controller.
    client
        .try_publish(&p.topic, qos(p.qos), p.retain, p.payload.as_bytes())
        .map_err(|_| SubmissionError::Rejected)
}
pub struct Session {
    pub client: AsyncClient,
    pub rx: mpsc::Receiver<Result<Event, ()>>,
    pub worker: tokio::task::JoinHandle<()>,
    pub progress: Arc<AtomicU64>,
    pub dispatched: Arc<AtomicU64>,
}
impl Session {
    pub fn start(
        c: &Config,
        secrets: &Secrets,
        id: &str,
        epoch: Instant,
        authorized: Arc<AtomicBool>,
    ) -> Self {
        let (client, mut eventloop) = connect(c, secrets, id);
        let (tx, rx) = mpsc::channel(128);
        let progress = Arc::new(AtomicU64::new(epoch.elapsed().as_millis() as u64));
        let pulse = progress.clone();
        let dispatched = Arc::new(AtomicU64::new(0));
        let sent = dispatched.clone();
        let worker = tokio::spawn(async move {
            loop {
                if !authorized.load(Ordering::Relaxed) {
                    return;
                }
                // Keep the poll future alive across progress ticks, including during DNS/connect.
                let event = eventloop.poll();
                tokio::pin!(event);
                let outcome = loop {
                    tokio::select! {
                        value=&mut event=>break value,
                        _=tokio::time::sleep(Duration::from_millis(250))=>{
                        if !authorized.load(Ordering::Relaxed) { return; }
                        pulse.store(epoch.elapsed().as_millis() as u64,Ordering::Relaxed);
                    }
                    }
                };
                pulse.store(epoch.elapsed().as_millis() as u64, Ordering::Relaxed);
                if matches!(&outcome, Ok(Event::Outgoing(rumqttc::Outgoing::Publish(_)))) {
                    sent.fetch_add(1, Ordering::Relaxed);
                }
                let failed = outcome.is_err();
                if tx.send(outcome.map_err(|_| ())).await.is_err() || failed {
                    break;
                }
            }
        });
        Self {
            client,
            rx,
            worker,
            progress,
            dispatched,
        }
    }
    pub fn stop(&self) {
        self.worker.abort();
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.worker.abort();
    }
}
// Explicit compatibility flag only. Signature verification remains intact; this disables trust/hostname checks.
#[derive(Debug)]
struct UnverifiedCertificate;
impl rustls::client::danger::ServerCertVerifier for UnverifiedCertificate {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}
