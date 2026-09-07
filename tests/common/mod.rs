#![allow(dead_code)]
use carstate::{
    config::{self, Config},
    engine::Engine,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
pub fn source() -> Value {
    json!({
    "http":{"use_http":false},"mqtt_settings":{"ip":"127.0.0.1"},
    "inputs":{"charging":{"topic":"car/status","true_values":["Charging"],"false_values":["Complete","Disconnected","Stopped"]},"plugged_in":{"topic":"car/status","true_values":["Charging","Complete","Stopped"],"false_values":["Disconnected"]},"charge_complete":{"topic":"car/status","true_values":["Complete"],"false_values":["Charging","Disconnected","Stopped"]},"parked":{"topic":"car/parked","true_values":["true"],"false_values":["false"]},"location":{"mode":"json","topic":"car/gps"},"battery":{"topic":"car/battery"},"locked":{"topic":"car/locked","true_values":["true"],"false_values":["false"]},"online":{"topic":"car/online","true_values":["true"],"false_values":["false"]},"source_healthy":{"topic":"car/health","true_values":["true"],"false_values":["false"]},"faults":[{"name":"tpms","topic":"car/fault","true_values":["true"],"false_values":["false"]}]},
    "state_settings":[{"name":"home","target_latitude":0.,"target_longitude":0.,"inner_radius_meters":50.,"outer_radius_meters":300.,"battery_low_percent":20.,"battery_low_is_fault":true}],
    "outputs":[
    {"name":"red","topic":"cmnd/test/POWER1","true_payload":"ON","false_payload":"OFF","state":"vehicle_fault"},
    {"name":"orange","topic":"cmnd/test/POWER2","true_payload":"ON","false_payload":"OFF","rules":[
    {"name":"fault","state":"vehicle_fault","priority":100,"behavior":{"macro":"steady","value":false}},
    {"name":"reminder","state":"location_inner_parked_not_plugged_in","priority":20,"behavior":{"macro":"blink_for","interval_seconds":4.,"duration_seconds":60.}},
    {"name":"baseline","state":"location_inner_parked_not_plugged_in","priority":10,"behavior":{"macro":"steady","value":true}}]},
    {"name":"green","topic":"cmnd/test/POWER3","true_payload":"ON","false_payload":"OFF","state":"location_inner_and_plugged_in"},
    {"name":"blue","topic":"cmnd/test/POWER4","true_payload":"ON","false_payload":"OFF","state":"location_outer_band"}
    ]})
}
pub fn config(value: Value) -> Config {
    config::parse(&value.to_string(), "{}", &BTreeMap::new())
        .unwrap()
        .0
}
pub fn engine() -> Engine {
    let mut c = config(source());
    c.jitter.max_changes = 1000;
    let mut e = Engine::new(c, false);
    connect(&mut e, 0.);
    e
}
pub fn connect(e: &mut Engine, t: f64) {
    e.connection(true, t);
    let topics = e.subscriptions.keys().cloned().collect::<Vec<_>>();
    let grants = vec![true; topics.len()];
    e.acknowledged(&topics, &grants, t);
}
pub fn feed(e: &mut Engine, t: f64, topic: &str, payload: &str) {
    e.receive(topic, payload.as_bytes(), t);
}
pub fn home(e: &mut Engine, t: f64, status: &str) {
    for (topic, payload) in [
        ("car/gps", r#"{"latitude":0,"longitude":0}"#),
        ("car/parked", "true"),
        ("car/fault", "false"),
        ("car/battery", "80"),
        ("car/status", status),
    ] {
        feed(e, t, topic, payload);
    }
}
pub fn tick(e: &mut Engine, t: f64) -> Vec<(String, String)> {
    let mut commands = Vec::new();
    e.tick(t, |p| {
        commands.push((p.topic.clone(), p.payload.clone()));
        Ok(())
    });
    commands
}
