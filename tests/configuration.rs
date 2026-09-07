mod common;
use carstate::{
    app_log,
    config::{self, LocationInput},
    model::State,
};
use common::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;
#[test]
fn defaults_and_minimal_optional_configuration() {
    let raw = json!({"inputs":{"charging":{"topic":"a","true_values":["true"],"false_values":["false"]}},"outputs":[{"name":"a","topic":"b","true_payload":"ON","false_payload":"OFF","state":"charging"}]});
    let c = config(raw);
    assert_eq!(c.jitter.max_changes, 3);
    assert_eq!(c.jitter.cooldown_seconds, 10.);
    assert_eq!(c.output_settings.min_hold_seconds, 1.);
    assert!(!c.heartbeat.enabled);
    assert!(!c.http.state_endpoint_enabled);
    assert!(c.state_settings.is_empty());
    assert_eq!(c.outputs[0].rules()[0].state, State::Charging);
}
#[test]
fn full_example_and_split_validate() {
    config::parse(
        include_str!("../config/carstate.json5"),
        include_str!("../config/secrets.example.json5"),
        &BTreeMap::new(),
    )
    .unwrap();
    let mut v = source();
    v["inputs"]["location"] = json!({"mode":"split","latitude_topic":"a","longitude_topic":"b"});
    assert!(matches!(
        config(v).inputs.location,
        Some(LocationInput::Split {
            max_coordinate_skew_seconds: 10.,
            ..
        })
    ));
}
#[test]
fn validation_matrix_rejects_invalid_cross_fields_and_types() {
    let cases: Vec<(&str, Value)> = vec![
        ("/http/port", json!(0)),
        ("/http/port", json!(65536)),
        ("/http/use_http", json!("true")),
        ("/http/state_endpoint_enabled", json!(true)),
        ("/http/state_endpoint_enabled", json!(1)),
        ("/mqtt_settings/port", json!(0)),
        ("/mqtt_settings/qos", json!(2)),
        ("/mqtt_settings/keep_alive_seconds", json!(0)),
        ("/mqtt_settings/publish_retry_initial_seconds", json!(0)),
        ("/mqtt_settings/publish_retry_max_seconds", json!(0.5)),
        ("/mqtt_settings/publish_timeout_seconds", json!(-1)),
        ("/state_settings", json!({})),
        ("/state_settings/0/name", json!("")),
        ("/state_settings/0/inner_radius_meters", json!(301)),
        ("/state_settings/0/outer_radius_meters", json!(-1)),
        ("/state_settings/0/target_latitude", json!(91)),
        ("/state_settings/0/target_longitude", json!(-181)),
        ("/state_settings/0/battery_low_percent", json!(101)),
        ("/state_settings/0/battery_low_is_fault", json!("yes")),
        ("/jitter/max_changes", json!(0)),
        ("/jitter/max_changes", json!(1.5)),
        ("/jitter/max_changes", json!(-1)),
        ("/jitter/cooldown_seconds", json!(0.9)),
        ("/output_settings/min_hold_seconds", json!(0.9)),
        ("/runtime_settings/worker_stall_seconds", json!(0)),
        ("/inputs/charging/topic", json!("a/#")),
        ("/inputs/charging/topic", json!("a/+")),
        ("/inputs/charging/topic", json!("a\u{0}")),
        ("/inputs/charging/topic", json!(" ")),
        ("/inputs/charging/true_values", json!([])),
        ("/inputs/charging/false_values", json!([" CHARGING "])),
        ("/inputs/charging/stale_after_seconds", json!(0)),
        ("/inputs/faults/0/name", json!("")),
        ("/inputs/faults/0/name", json!("bad name")),
        ("/outputs/0/state", json!("healthy")),
        ("/outputs/0/true_payload", json!("")),
        ("/outputs/0/false_payload", json!("TOGGLE")),
        ("/outputs/0/topic", json!("car/gps")),
        ("/outputs/0/device", json!("missing")),
        ("/outputs/0/name", json!("")),
        ("/outputs/1/rules", json!([])),
        ("/outputs/1/rules/0/priority", json!(20)),
        ("/outputs/1/rules/0/name", json!("reminder")),
        ("/outputs/1/rules/0/priority", json!(1.5)),
        ("/outputs/1/rules/0/behavior/macro", json!("script")),
        ("/outputs/1/rules/1/behavior/interval_seconds", json!(1.9)),
        ("/outputs/1/rules/1/behavior/duration_seconds", json!(3)),
        ("/heartbeat/enabled", json!("true")),
        ("/heartbeat/interval_seconds", json!(1.5)),
        ("/heartbeat/timeout_seconds", json!(155)),
        ("/heartbeat/timeout_seconds", json!(65536)),
    ];
    for (path, value) in cases {
        let mut raw = source();
        set(&mut raw, path, value);
        assert!(
            config::parse(&raw.to_string(), "{}", &BTreeMap::new()).is_err(),
            "accepted {path}: {raw}"
        );
    }
    for kind in ["state_settings", "outputs", "output_devices"] {
        let mut raw = source();
        if kind == "output_devices" {
            raw[kind] = json!([{"name":"light","availability":{"topic":"lwt","true_values":["Online"],"false_values":["Offline"]}}]);
        }
        let duplicate = raw[kind][0].clone();
        raw[kind].as_array_mut().unwrap().push(duplicate);
        assert!(config::parse(&raw.to_string(), "{}", &BTreeMap::new()).is_err());
    }
    let mut v = source();
    v["outputs"][0]["rules"] = json!([]);
    assert!(config::parse(&v.to_string(), "{}", &BTreeMap::new()).is_err());
    for n in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1e30] {
        assert!(config::seconds(n, 1., "test").is_err());
    }
}
fn set(v: &mut Value, path: &str, value: Value) {
    let (parent, key) = path.rsplit_once('/').unwrap();
    if v.pointer(parent).is_none() {
        v[parent.trim_start_matches('/')] = json!({});
    }
    v.pointer_mut(parent)
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert(key.into(), value);
}
#[test]
fn exact_nested_unknown_paths_and_vehicle_extras_are_separate() {
    let mut v = source();
    v["state_settings"][0]["battery_low_precent"] = json!(20);
    v["outputs"][1]["rules"][1]["behavior"]["duraton"] = json!(2);
    v["extension"] = json!({"ignored":true});
    let warnings = config::parse(&v.to_string(), "{}", &BTreeMap::new())
        .unwrap()
        .2;
    assert_eq!(warnings.len(), 3);
    assert!(warnings
        .contains(&"Unknown configuration key: state_settings[0].battery_low_precent".into()));
    assert!(warnings
        .contains(&"Unknown configuration key: outputs[1].rules[1].behavior.duraton".into()));
}
#[test]
fn environment_references_and_compatibility_precedence() {
    let env = BTreeMap::from([
        ("BROKER".into(), " example ".into()),
        ("EMPTY".into(), "".into()),
        ("PORT".into(), "3200".into()),
        ("CARSTATE_HTTP_PORT".into(), "3400".into()),
        ("CARSTATE_USE_HTTP".into(), "false".into()),
    ]);
    let mut v = source();
    v["mqtt_settings"]["ip"] = json!("${BROKER}");
    v["http"]["port"] = json!("${PORT}");
    let (c, s, _) = config::parse(
        &v.to_string(),
        "{mqtt_password:'${EMPTY}',mqtt_client_id:'prefix-${BROKER}'}",
        &env,
    )
    .unwrap();
    assert_eq!(c.mqtt_settings.ip, " example ");
    assert_eq!(c.http.port, 3400);
    assert_eq!(s.mqtt_password, "");
    assert_eq!(s.mqtt_client_id, "prefix-${BROKER}");
    v["jitter"] = json!({"max_changes":"${PORT}"});
    assert!(config::parse(&v.to_string(), "{}", &env).is_err());
    v = source();
    v["http"]["use_http"] = json!(true);
    v["http"]["state_endpoint_enabled"] = json!(true);
    assert!(config::parse(&v.to_string(), "{http_state_token:'secret'}", &env).is_err());
    let mut nested = json!({"a":["${EMPTY}","${MISSING}",{"${BROKER}":"${BROKER}"}]});
    styleguide_logger::config::resolve_environment_references(&mut nested, &|k| {
        env.get(k).cloned()
    });
    assert_eq!(nested["a"][0], "");
    assert!(nested["a"][1].is_null());
    assert_eq!(nested["a"][2]["${BROKER}"], " example ");
}
#[test]
fn id_generation_is_injectable_and_preserves_explicit_values() {
    assert_eq!(
        app_log::client_id(" fixed ", false, |_| panic!()).unwrap(),
        (" fixed ".into(), false)
    );
    for input in ["", "   "] {
        let (id, generated) = app_log::client_id(input, false, |b| {
            b.fill(1);
            Ok(())
        })
        .unwrap();
        assert_eq!(id, "carstate_BBBBBBBB");
        assert!(generated);
    }
    assert_eq!(
        app_log::client_id("live", true, |b| {
            b.fill(35);
            Ok(())
        })
        .unwrap()
        .0,
        "carstate_dryrun_99999999"
    );
    assert!(app_log::client_id("", false, |_| Err("rng_failed".into())).is_err());
}

#[test]
fn split_device_heartbeat_collisions_and_incomplete_geofences_are_validated() {
    let invalid =
        |raw: Value| assert!(config::parse(&raw.to_string(), "{}", &BTreeMap::new()).is_err());
    let mut v = source();
    v["inputs"]["location"] =
        json!({"mode":"split","latitude_topic":"same","longitude_topic":"same"});
    invalid(v);
    let mut v = source();
    v["inputs"]["location"] = json!({"mode":"split","latitude_topic":"a","longitude_topic":"b","max_coordinate_skew_seconds":0});
    invalid(v);
    let mut v = source();
    v["state_settings"][0]
        .as_object_mut()
        .unwrap()
        .remove("target_longitude");
    invalid(v);
    let mut v = source();
    let duplicate = v["inputs"]["faults"][0].clone();
    v["inputs"]["faults"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    invalid(v);
    let mut v = source();
    v["inputs"] = json!({});
    invalid(v);
    let mut v = source();
    v["outputs"] = json!([]);
    invalid(v);
    for topic in ["car/status", "cmnd/test/POWER1", "+", ""] {
        let mut v = source();
        v["heartbeat"] = json!({"enabled":true,"topic":topic,"payload":"alive"});
        invalid(v);
    }
    let mut v = source();
    v["heartbeat"] = json!({"enabled":true,"topic":"heartbeat","payload":""});
    invalid(v);
    let mut v = source();
    v["state_settings"] = json!([{ "name":"battery_only","battery_low_percent":20 }]);
    let c = config(v);
    assert_eq!(c.state_settings.len(), 1);
    assert!(c.state_settings[0].target_latitude.is_none());
}

#[test]
fn malformed_sections_fail_before_compatibility_override_indexing() {
    for http in [json!(true), json!(17), json!("secret-value"), json!(null)] {
        let mut v = source();
        v["http"] = http;
        let result = config::parse(
            &v.to_string(),
            "{}",
            &BTreeMap::from([("CARSTATE_HTTP_PORT".into(), "3000".into())]),
        );
        assert!(result.is_err());
    }
}
