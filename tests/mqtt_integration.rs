//! Local MQTT wire simulator: never connects to a configured broker or physical relay.
mod common;
use carstate::{
    app_log::{AppLog, Identity},
    config::Secrets,
    engine::{Engine, SnapshotIdentity},
    model::{BuildInfo, Clock},
    runtime::Runtime,
};
use common::*;
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{atomic::AtomicBool, Arc, RwLock},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::watch,
};
async fn frame(stream: &mut TcpStream) -> (u8, Vec<u8>) {
    let header = stream.read_u8().await.unwrap();
    let mut length = 0usize;
    let mut multiplier = 1;
    loop {
        let b = stream.read_u8().await.unwrap();
        length += (b as usize & 127) * multiplier;
        if b < 128 {
            break;
        }
        multiplier *= 128;
        assert!(multiplier <= 128 * 128 * 128);
    }
    let mut body = vec![0; length];
    stream.read_exact(&mut body).await.unwrap();
    (header, body)
}
async fn send(stream: &mut TcpStream, header: u8, body: &[u8]) {
    let mut bytes = vec![header];
    let mut length = body.len();
    loop {
        let mut b = (length % 128) as u8;
        length /= 128;
        if length > 0 {
            b |= 128;
        }
        bytes.push(b);
        if length == 0 {
            break;
        }
    }
    bytes.extend_from_slice(body);
    stream.write_all(&bytes).await.unwrap();
}
fn string(body: &[u8], offset: &mut usize) -> String {
    let size = u16::from_be_bytes([body[*offset], body[*offset + 1]]) as usize;
    *offset += 2;
    let result = String::from_utf8(body[*offset..*offset + size].to_vec()).unwrap();
    *offset += size;
    result
}
async fn publish(stream: &mut TcpStream, topic: &str, payload: &str) {
    let mut body = (topic.len() as u16).to_be_bytes().to_vec();
    body.extend_from_slice(topic.as_bytes());
    body.extend_from_slice(payload.as_bytes());
    send(stream, 0x30, &body).await;
}
async fn handshake(listener: &TcpListener) -> (TcpStream, String) {
    let (mut s, _) = listener.accept().await.unwrap();
    let (h, b) = frame(&mut s).await;
    assert_eq!(h, 0x10);
    assert_eq!(b[7] & 0b00000100, 0, "Last Will forbidden");
    assert_ne!(b[7] & 2, 0, "clean session required");
    let mut offset = 10;
    let id = string(&b, &mut offset);
    send(&mut s, 0x20, &[0, 0]).await;
    let (h, b) = frame(&mut s).await;
    assert_eq!(h, 0x82);
    let mut cursor = 2;
    let mut topics = BTreeSet::new();
    let mut grants = b[..2].to_vec();
    while cursor < b.len() {
        let topic = string(&b, &mut cursor);
        assert!(topics.insert(topic), "subscriptions must be deduplicated");
        grants.push(b[cursor]);
        cursor += 1;
    }
    send(&mut s, 0x90, &grants).await;
    for (t, p) in [
        ("car/gps", r#"{"lat":0,"lng":0}"#),
        ("car/parked", "true"),
        ("car/battery", "80"),
        ("car/fault", "false"),
        ("car/status", "Complete"),
    ] {
        publish(&mut s, t, p).await;
    }
    (s, id)
}
async fn commands(stream: &mut TcpStream, count: usize) -> BTreeMap<String, String> {
    let mut result = BTreeMap::new();
    while result.len() < count {
        let (header, body) = frame(stream).await;
        match header >> 4 {
            3 => {
                let mut offset = 0;
                let topic = string(&body, &mut offset);
                let qos = (header >> 1) & 3;
                if qos == 1 {
                    let id = [body[offset], body[offset + 1]];
                    offset += 2;
                    send(stream, 0x40, &id).await;
                }
                assert_eq!(header & 1, 0);
                result.insert(topic, String::from_utf8(body[offset..].to_vec()).unwrap());
            }
            12 => send(stream, 0xd0, &[]).await,
            _ => panic!("unexpected packet {header}"),
        }
    }
    result
}
async fn launch(
    port: u16,
    dry: bool,
) -> (
    watch::Sender<bool>,
    tokio::task::JoinHandle<Result<(), String>>,
    Arc<RwLock<carstate::engine::Snapshot>>,
) {
    let mut v = source();
    v["mqtt_settings"]["port"] = json!(port);
    let c = config(v);
    let identity = SnapshotIdentity {
        build: BuildInfo::default(),
        client_id: if dry {
            "carstate_dryrun_TEST0001"
        } else {
            "carstate_TEST0001"
        }
        .into(),
        generated: true,
        credentials: false,
    };
    let clock = Clock::default();
    let snapshot = Arc::new(RwLock::new(
        Engine::new(c.clone(), dry).snapshot(0., &clock, &identity),
    ));
    let (log, _) = AppLog::start(
        styleguide_logger::Logger::new(
            styleguide_logger::LoggingConfig::from_json5_with_environment(
                "{sinks:{console:{enabled:false}}}",
                &|_| None,
            )
            .unwrap(),
            BTreeMap::new(),
        )
        .unwrap(),
        Identity::new("test", &BTreeMap::new()),
    );
    let runtime = Runtime {
        config: c,
        secrets: Secrets::default(),
        identity,
        clock,
        dry,
        snapshot: snapshot.clone(),
        workers_ok: Arc::new(AtomicBool::new(true)),
        log,
    };
    let (tx, rx) = watch::channel(false);
    (tx, tokio::spawn(runtime.run(rx)), snapshot)
}
#[tokio::test]
async fn mqtt_inputs_drive_all_four_power_topics_and_reconnect_reuses_identity() {
    tokio::time::timeout(Duration::from_secs(12), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (stop, worker, snapshot) = launch(listener.local_addr().unwrap().port(), false).await;
        let (mut connection, id) = handshake(&listener).await;
        let actual = commands(&mut connection, 4).await;
        assert_eq!(
            actual,
            BTreeMap::from([
                ("cmnd/test/POWER1".into(), "OFF".into()),
                ("cmnd/test/POWER2".into(), "OFF".into()),
                ("cmnd/test/POWER3".into(), "ON".into()),
                ("cmnd/test/POWER4".into(), "OFF".into())
            ])
        );
        publish(&mut connection, "car/status", "Disconnected").await;
        let changes = commands(&mut connection, 2).await;
        assert_eq!(changes["cmnd/test/POWER2"], "ON");
        assert_eq!(changes["cmnd/test/POWER3"], "OFF");
        drop(connection);
        let (mut recovered, recovered_id) = handshake(&listener).await;
        assert_eq!(recovered_id, id);
        assert_eq!(commands(&mut recovered, 4).await.len(), 4);
        assert_eq!(snapshot.read().unwrap().mqtt["connection_generation"], 2);
        stop.send(true).unwrap();
        assert!(worker.await.unwrap().is_ok());
    })
    .await
    .expect("MQTT integration timeout");
}
#[tokio::test]
async fn dry_run_subscribes_and_evaluates_but_has_zero_publish_packets() {
    tokio::time::timeout(Duration::from_secs(6), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (stop, worker, snapshot) = launch(listener.local_addr().unwrap().port(), true).await;
        let (mut connection, id) = handshake(&listener).await;
        assert!(id.starts_with("carstate_dryrun_"));
        assert!(
            tokio::time::timeout(Duration::from_millis(1500), frame(&mut connection))
                .await
                .is_err(),
            "dry-run sent a packet"
        );
        {
            let s = snapshot.read().unwrap();
            assert_eq!(s.runtime["mode"], "dry_run");
            assert_eq!(s.runtime["publishing_enabled"], false);
            assert_eq!(s.states["charge_complete"]["value"], true);
            assert_eq!(s.outputs[2]["submission_count"], 0);
            assert_eq!(s.outputs[2]["would_publish"]["count"], 1);
        }
        stop.send(true).unwrap();
        assert!(worker.await.unwrap().is_ok());
    })
    .await
    .expect("dry-run integration timeout");
}
