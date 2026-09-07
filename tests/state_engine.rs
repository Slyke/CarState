mod common;
use carstate::{
    engine::{Engine, SnapshotIdentity},
    inputs::Vehicle,
    model::{and, not, or, Location, State},
    model::{BuildInfo, Clock},
    rules,
};
use common::*;
use serde_json::json;
#[test]
fn three_valued_truth_tables() {
    for a in [None, Some(false), Some(true)] {
        assert_eq!(not(not(a)), a);
        for b in [None, Some(false), Some(true)] {
            assert_eq!(and([a, b]), and([b, a]));
            assert_eq!(or([a, b]), or([b, a]));
        }
    }
    assert_eq!(and([Some(false), None]), Some(false));
    assert_eq!(and([Some(true), None]), None);
    assert_eq!(or([Some(true), None]), Some(true));
    assert_eq!(or([Some(false), None]), None);
    assert_eq!(or([]), None);
}
#[test]
fn charging_completion_is_atomic_and_keeps_green_connected() {
    let mut e = engine();
    home(&mut e, 0., "Charging");
    tick(&mut e, 1.);
    assert_eq!(e.states[&State::Charging].value, Some(true));
    assert_eq!(e.outputs[2].last_submitted, Some(true));
    for (t, status, charging, plugged, complete) in [
        (2., "Complete", false, true, true),
        (3., "Stopped", false, true, false),
        (4., "Disconnected", false, false, false),
        (5., "Charging", true, true, false),
    ] {
        feed(&mut e, t, "car/status", status);
        tick(&mut e, t);
        assert_eq!(e.states[&State::Charging].value, Some(charging));
        assert_eq!(e.states[&State::PluggedIn].value, Some(plugged));
        assert_eq!(e.states[&State::ChargeComplete].value, Some(complete));
        assert_eq!(
            e.states[&State::LocationInnerParkedNotPluggedIn].value,
            Some(!plugged)
        );
    }
}
#[test]
fn malformed_inputs_preserve_values_and_receipts_and_duplicates_refresh_only_receipts() {
    let mut e = engine();
    home(&mut e, 0., "Complete");
    tick(&mut e, 1.);
    let changed = e.vehicle.booleans["charging"].current.last_changed;
    feed(&mut e, 3., "car/status", " complete ");
    tick(&mut e, 3.);
    assert_eq!(e.vehicle.booleans["charging"].current.last_changed, changed);
    assert_eq!(e.vehicle.booleans["charging"].received, Some(3.));
    for (topic, payload) in [
        ("car/status", "unknown"),
        ("car/gps", "{}"),
        ("car/gps", r#"{"lat":91,"lng":0}"#),
        ("car/battery", "NaN"),
        ("car/battery", "101"),
    ] {
        feed(&mut e, 4., topic, payload);
    }
    e.receive("car/status", &[255], 4.);
    tick(&mut e, 4.);
    assert_eq!(e.vehicle.booleans["charging"].received, Some(3.));
    assert_eq!(e.vehicle.battery.current.value, Some(80.));
    assert_eq!(e.counters.rejected, 6);
    feed(
        &mut e,
        5.,
        "car/gps",
        r#"{"lat":0,"lng":0,"extra":{"anything":true}}"#,
    );
    tick(&mut e, 5.);
    assert_eq!(e.counters.rejected, 6);
}
#[test]
fn each_fixed_state_covers_known_and_unknown_results() {
    let mut e = engine();
    tick(&mut e, 0.);
    for s in State::ALL {
        assert_eq!(e.states[&s].value, None, "{s:?}");
    }
    let mut seen = std::collections::BTreeMap::<State, std::collections::BTreeSet<bool>>::new();
    let mut time = 1.;
    for gps in [
        r#"{"lat":0,"lng":0}"#,
        r#"{"lat":0.001,"lng":0}"#,
        r#"{"lat":1,"lng":0}"#,
    ] {
        for status in ["Charging", "Complete", "Disconnected"] {
            for flag in ["true", "false"] {
                for battery in ["10", "90"] {
                    for (topic, payload) in [
                        ("car/gps", gps),
                        ("car/status", status),
                        ("car/parked", flag),
                        ("car/locked", flag),
                        ("car/online", flag),
                        ("car/health", flag),
                        ("car/fault", flag),
                        ("car/battery", battery),
                    ] {
                        feed(&mut e, time, topic, payload);
                    }
                    tick(&mut e, time);
                    for s in State::ALL {
                        if let Some(v) = e.states[&s].value {
                            seen.entry(s).or_default().insert(v);
                        }
                    }
                    time += 11.;
                }
            }
        }
    }
    for s in State::ALL {
        assert_eq!(seen[&s].len(), 2, "{s:?}");
    }
}
#[test]
fn location_overlaps_and_battery_fault_flags_remain_per_entry() {
    let mut v = source();
    v["state_settings"].as_array_mut().unwrap().push(json!({"name":"second","target_latitude":0.001,"target_longitude":0.,"inner_radius_meters":10.,"outer_radius_meters":500.,"battery_low_percent":30.,"battery_low_is_fault":false}));
    let mut e = Engine::new(config(v), false);
    connect(&mut e, 0.);
    home(&mut e, 0., "Complete");
    feed(&mut e, 0., "car/battery", "25");
    tick(&mut e, 1.);
    assert_eq!(e.states[&State::LocationInner].value, Some(true));
    assert_eq!(e.states[&State::LocationOuterBand].value, Some(true));
    assert_eq!(e.states[&State::BatteryLow].value, Some(true));
    assert_eq!(e.states[&State::VehicleFault].value, Some(false));
    let old = e.states[&State::LocationInner].last_changed;
    feed(&mut e, 2., "car/gps", r#"{"lat":0.001,"lng":0}"#);
    let commands = tick(&mut e, 2.);
    assert_eq!(e.states[&State::LocationInner].last_changed, old);
    assert!(!commands.iter().any(|(topic, _)| topic.ends_with("POWER3")));
    let (before, _) = rules::evaluate(&e.config, &e.vehicle);
    e.config.state_settings.reverse();
    assert_eq!(before, rules::evaluate(&e.config, &e.vehicle).0);
}
#[test]
fn source_health_is_independent_and_battery_omission_is_not_an_unknown_fault_source() {
    let mut v = source();
    v["inputs"].as_object_mut().unwrap().remove("battery");
    let mut e = Engine::new(config(v), false);
    connect(&mut e, 0.);
    home(&mut e, 0., "Complete");
    feed(&mut e, 0., "car/health", "false");
    tick(&mut e, 1.);
    assert_eq!(e.states[&State::VehicleFault].value, Some(false));
    assert_eq!(e.states[&State::SourceHealthy].value, Some(false));
    assert!(e.healthy());
    feed(&mut e, 2., "car/fault", "true");
    feed(&mut e, 2., "car/health", "true");
    tick(&mut e, 2.);
    assert_eq!(e.states[&State::VehicleFault].value, Some(true));
}
#[test]
fn haversine_target_boundaries_and_antipodes() {
    let a = Location {
        latitude: 0.,
        longitude: 0.,
    };
    assert_eq!(rules::distance(a, a), 0.);
    for meters in [50., 300., 301.] {
        let b = Location {
            latitude: (meters / 6_371_000_f64).to_degrees(),
            longitude: 0.,
        };
        assert!((rules::distance(a, b) - meters).abs() < 1e-7);
    }
    assert!(
        (rules::distance(
            a,
            Location {
                latitude: 0.,
                longitude: 180.
            }
        ) - std::f64::consts::PI * 6_371_000.)
            .abs()
            < 1e-7
    );
}
#[test]
fn freshness_expiry_and_recovery_without_new_value_changes() {
    let mut v = source();
    v["inputs"]["source_healthy"]["stale_after_seconds"] = json!(5);
    let mut e = Engine::new(config(v), false);
    connect(&mut e, 0.);
    feed(&mut e, 0., "car/health", "true");
    tick(&mut e, 0.);
    feed(&mut e, 4., "car/health", "true");
    tick(&mut e, 5.);
    assert_eq!(
        e.vehicle.booleans["source_healthy"].current.last_changed,
        Some(0.)
    );
    feed(&mut e, 8., "car/health", "bad");
    tick(&mut e, 9.);
    assert_eq!(e.states[&State::SourceHealthy].value, None);
    assert_eq!(e.vehicle.booleans["source_healthy"].reason, Some("stale"));
    assert!(e.healthy());
    feed(&mut e, 10., "car/health", "true");
    tick(&mut e, 10.);
    assert_eq!(
        e.vehicle.booleans["source_healthy"].current.last_changed,
        Some(10.)
    );
}
#[test]
fn split_gps_needs_both_new_axes_and_cannot_extend_incomplete_deadline() {
    let mut v = source();
    v["inputs"]["location"] = json!({"mode":"split","latitude_topic":"lat","longitude_topic":"lon","max_coordinate_skew_seconds":10,"stale_after_seconds":20});
    let c = config(v);
    let mut vehicle = Vehicle::new(&c.inputs);
    vehicle.receive(&c.inputs, "lat", b"0", 0.);
    assert!(vehicle.location.current.value.is_none());
    vehicle.receive(&c.inputs, "lon", b"0", 1.);
    assert_eq!(vehicle.location.received, Some(0.));
    vehicle.receive(&c.inputs, "lat", b"1", 2.);
    assert_eq!(vehicle.location.current.value.unwrap().latitude, 0.);
    vehicle.receive(&c.inputs, "lat", b"2", 11.);
    vehicle.expire(12.);
    assert_eq!(vehicle.location.reason, Some("incoherent"));
    vehicle.receive(&c.inputs, "lon", b"3", 13.);
    assert_eq!(
        vehicle.location.current.value.unwrap(),
        Location {
            latitude: 2.,
            longitude: 3.
        }
    );
    assert_eq!(vehicle.location.received, Some(11.));
    vehicle.receive(&c.inputs, "lon", b"3", 14.);
    vehicle.receive(&c.inputs, "lat", b"2", 14.);
    assert_eq!(vehicle.location.received, Some(14.));
    vehicle.expire(34.);
    assert_eq!(vehicle.location.reason, Some("stale"));
}
#[test]
fn snapshot_is_complete_allowlisted_and_read_only() {
    let mut e = engine();
    home(&mut e, 0., "Complete");
    tick(&mut e, 1.);
    let identity = SnapshotIdentity {
        build: BuildInfo::default(),
        client_id: "id".into(),
        generated: true,
        credentials: true,
    };
    let clock = Clock::default();
    let a = serde_json::to_value(e.snapshot(2., &clock, &identity)).unwrap();
    let b = serde_json::to_value(e.snapshot(2., &clock, &identity)).unwrap();
    assert_eq!(a, b);
    assert_eq!(a["states"].as_object().unwrap().len(), 20);
    assert_eq!(a["facts"]["locked"]["value"], serde_json::Value::Null);
    assert!(a["mqtt"].get("password").is_none());
    assert!(a["mqtt"].get("username").is_none());
    assert_eq!(a["entry_states"]["home"]["location_inner"]["value"], true);
}
