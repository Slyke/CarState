mod common;

use carstate::{
    config,
    engine::{Engine, SnapshotIdentity},
    model::{BuildInfo, Clock, State},
};
use common::{connect, feed, tick};
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn example_engine() -> Engine {
    let (mut config, _, warnings) = config::parse(
        include_str!("../config/carstate.example.json5"),
        "{}",
        &BTreeMap::new(),
    )
    .unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    // Exercise the light policy independently of the separately tested jitter budget.
    config.jitter.max_changes = 1000;
    let mut e = Engine::new(config, false);
    connect(&mut e, 0.);
    feed(&mut e, 0., "tele/garage-light/LWT", "Online");
    e
}

fn position(e: &mut Engine, now: f64, meters: f64) {
    let home = &e.config.state_settings[0];
    let payload = json!({
        "latitude":home.target_latitude.unwrap() + (meters / 6_371_000.).to_degrees(),
        "longitude":home.target_longitude.unwrap(),
    });
    feed(e, now, "vehicle/location", &payload.to_string());
}

fn seed(e: &mut Engine, meters: f64, parked: bool, status: &str) {
    position(e, 0., meters);
    for (topic, payload) in [
        ("vehicle/parked", if parked { "true" } else { "false" }),
        ("vehicle/charging_state", status),
        ("vehicle/fault", "healthy"),
        ("vehicle/tpms_soft_warning_fl", "false"),
        ("vehicle/battery_level", "80"),
    ] {
        feed(e, 0., topic, payload);
    }
    tick(e, 0.);
    tick(e, 1.);
}

fn lights(e: &Engine) -> [bool; 4] {
    std::array::from_fn(|i| e.outputs[i].last_submitted.expect("known output"))
}

fn snapshot(e: &Engine, now: f64) -> Value {
    let clock = Clock {
        started: chrono::DateTime::parse_from_rfc3339("2026-09-07T00:00:00Z")
            .unwrap()
            .into(),
        ..Clock::default()
    };
    serde_json::to_value(e.snapshot(
        now,
        &clock,
        &SnapshotIdentity {
            build: BuildInfo::default(),
            client_id: "test".into(),
            generated: false,
            credentials: false,
        },
    ))
    .unwrap()
}

#[test]
fn example_policy_covers_location_parked_and_all_charge_statuses() {
    // Expected order: red, orange/amber, green, blue.
    for (meters, parked, status, expected) in [
        (400., true, "Disconnected", [true, false, false, false]),
        (100., true, "Disconnected", [false, true, false, false]),
        (0., false, "Disconnected", [false, true, true, false]),
        (0., true, "Disconnected", [false, false, true, false]),
        (0., true, "Stopped", [false, false, true, false]),
        (0., true, "Complete", [false, false, true, false]),
        (0., true, "NoPower", [false, false, true, false]),
        (0., true, "Charging", [false, false, true, true]),
        (400., true, "Charging", [true, false, false, true]),
        (100., true, "Charging", [false, true, false, true]),
    ] {
        let mut e = example_engine();
        seed(&mut e, meters, parked, status);
        assert_eq!(lights(&e), expected, "{meters}/{parked}/{status}");
    }
}

#[test]
fn example_policy_supports_teslamate_topics_without_parked_input() {
    let (mut config, _, warnings) = config::parse(
        include_str!("../config/carstate.example.json5"),
        "{}",
        &BTreeMap::new(),
    )
    .unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    // Model a deployment with independent TeslaMate facts and a Tasmota topic prefix.
    // No developer configuration or secrets are needed to run this regression.
    config.inputs.location = Some(config::LocationInput::Json {
        topic: "teslamate/cars/1/location".into(),
        stale_after_seconds: None,
    });
    config.inputs.charging.as_mut().unwrap().topic = "teslamate/cars/1/charging_state".into();
    config.inputs.charge_complete.as_mut().unwrap().topic =
        "teslamate/cars/1/charging_state".into();
    config.inputs.plugged_in = Some(config::BooleanInput {
        topic: "teslamate/cars/1/plugged_in".into(),
        true_values: vec!["true".into()],
        false_values: vec!["false".into()],
        stale_after_seconds: None,
    });
    config.inputs.parked = None;
    config.output_devices[0].availability.topic = "tasmota/tele/example-light/LWT".into();
    for (i, output) in config.outputs.iter_mut().enumerate() {
        output.topic = format!("tasmota/cmnd/example-light/POWER{}", i + 1);
    }
    config::validate(&config, &config::Secrets::default()).unwrap();
    let mut e = Engine::new(config, false);
    connect(&mut e, 0.);
    feed(&mut e, 0., "tasmota/tele/example-light/LWT", "Online");
    let location = json!({"latitude":e.config.state_settings[0].target_latitude,"longitude":e.config.state_settings[0].target_longitude});
    feed(
        &mut e,
        0.,
        "teslamate/cars/1/location",
        &location.to_string(),
    );
    feed(&mut e, 0., "teslamate/cars/1/plugged_in", "true");
    feed(&mut e, 0., "teslamate/cars/1/charging_state", "Complete");
    tick(&mut e, 0.);
    tick(&mut e, 1.);
    assert_eq!(e.outputs[3].last_submitted, Some(false));
    feed(&mut e, 2., "teslamate/cars/1/charging_state", "Charging");
    let publications = tick(&mut e, 2.);
    assert!(publications.contains(&("tasmota/cmnd/example-light/POWER4".into(), "ON".into())));
    assert_eq!(e.outputs[2].last_submitted, Some(true));
    // Missing parked telemetry does not prevent charging indication.
    assert_eq!(e.states[&State::Parked].value, None);
    feed(&mut e, 4., "teslamate/cars/1/charging_state", "Complete");
    let publications = tick(&mut e, 4.);
    assert!(publications.contains(&("tasmota/cmnd/example-light/POWER4".into(), "OFF".into())));
    assert_eq!(e.outputs[2].last_submitted, Some(true));
}

#[test]
fn green_and_amber_share_five_minute_deadline_without_phase_history() {
    let mut e = example_engine();
    seed(&mut e, 0., true, "Disconnected");
    let history = snapshot(&e, 1.)["state_history"]["entries"].clone();
    for t in 2..300 {
        if t == 50 {
            // Duplicate status and GPS movement inside the same area are not state changes.
            feed(&mut e, 50., "vehicle/charging_state", "Disconnected");
            feed(&mut e, 50., "vehicle/battery_level", "81");
            position(&mut e, 50., 3.);
        }
        tick(&mut e, t as f64);
        assert_eq!(lights(&e), [false, false, (t / 2) % 2 == 0, false]);
    }
    assert_eq!(snapshot(&e, 299.)["state_history"]["entries"], history);
    tick(&mut e, 300.);
    assert_eq!(lights(&e), [false, true, true, false]);
    let end = snapshot(&e, 300.);
    let entries = end["state_history"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), history.as_array().unwrap().len() + 1);
    assert!(entries
        .last()
        .unwrap()
        .to_string()
        .contains("output_behavior_expired"));
    assert_eq!(e.outputs[1].episodes[0].deadline, Some(300.));
    assert_eq!(e.outputs[2].episodes[0].deadline, Some(300.));
    // An enabled heartbeat may resubmit steady targets; it must not cycle the lights
    // or add logical history just because it resynchronizes the device.
    let publications = tick(&mut e, 500.);
    assert_eq!(lights(&e), [false, true, true, false]);
    for (topic, payload) in publications {
        if let Some(output) = e.config.outputs.iter().find(|o| o.topic == topic) {
            let expected = output.name == "orange" || output.name == "green";
            assert_eq!(payload, output.payload(expected));
        }
    }
    assert_eq!(snapshot(&e, 500.)["state_history"], end["state_history"]);
}

#[test]
fn plugging_in_or_leaving_park_cancels_and_rearms_the_green_reminder() {
    let mut e = example_engine();
    seed(&mut e, 0., true, "Disconnected");
    feed(&mut e, 10., "vehicle/charging_state", "Complete");
    tick(&mut e, 10.);
    assert_eq!(lights(&e), [false, false, true, false]);
    feed(&mut e, 20., "vehicle/charging_state", "Disconnected");
    tick(&mut e, 20.);
    assert_eq!(e.outputs[2].episodes[0].deadline, Some(320.));
    feed(&mut e, 30., "vehicle/parked", "false");
    tick(&mut e, 30.);
    assert_eq!(lights(&e), [false, true, true, false]);
    feed(&mut e, 40., "vehicle/parked", "true");
    tick(&mut e, 40.);
    assert_eq!(lights(&e), [false, false, true, false]);
    assert_eq!(e.outputs[1].episodes[0].deadline, Some(340.));
    assert_eq!(e.outputs[2].episodes[0].deadline, Some(340.));
}

#[test]
fn fault_blinks_alongside_other_colors_and_restarts_on_unplug_and_arrival() {
    let mut e = example_engine();
    seed(&mut e, 0., true, "Charging");
    feed(&mut e, 10., "vehicle/fault", "low tyre pressure");
    tick(&mut e, 10.);
    assert_eq!(lights(&e), [true, false, true, true]);
    tick(&mut e, 12.);
    assert_eq!(lights(&e), [false, false, true, true]);
    tick(&mut e, 310.);
    assert_eq!(lights(&e), [false, false, true, true]);
    feed(&mut e, 320., "vehicle/charging_state", "Disconnected");
    tick(&mut e, 320.);
    assert_eq!(lights(&e), [true, false, true, false]);
    assert_eq!(e.outputs[0].episodes[0].deadline, Some(620.));
    feed(&mut e, 321., "vehicle/charging_state", "Disconnected");
    tick(&mut e, 321.);
    assert_eq!(e.outputs[0].episodes[0].deadline, Some(620.));
    position(&mut e, 330., 100.);
    tick(&mut e, 330.);
    assert_eq!(lights(&e), [false, true, false, false]);
    assert_eq!(e.outputs[0].episodes[0].deadline, Some(620.));
    position(&mut e, 340., 0.);
    tick(&mut e, 340.);
    assert_eq!(lights(&e), [true, false, true, false]);
    assert_eq!(e.outputs[0].episodes[0].deadline, Some(640.));
    feed(&mut e, 350., "vehicle/fault", "healthy");
    tick(&mut e, 350.);
    assert!(!lights(&e)[0]);
    assert!(e.outputs[0].episodes[0].start.is_none());
}

#[test]
fn fault_preempts_solid_away_red_then_returns_to_it_and_blue_stays_on() {
    let mut e = example_engine();
    seed(&mut e, 400., true, "Charging");
    feed(&mut e, 10., "vehicle/battery_level", "20");
    tick(&mut e, 10.);
    tick(&mut e, 12.);
    assert_eq!(lights(&e), [false, false, false, true]);
    tick(&mut e, 310.);
    assert_eq!(lights(&e), [true, false, false, true]);
    assert_eq!(e.states[&State::VehicleFault].value, Some(true));
}

#[test]
fn unknown_unplug_state_and_duplicates_do_not_restart_an_exhausted_fault() {
    let mut e = example_engine();
    seed(&mut e, 0., true, "Disconnected");
    feed(&mut e, 10., "vehicle/fault", "low tyre pressure");
    tick(&mut e, 10.);
    tick(&mut e, 310.);
    e.vehicle
        .booleans
        .get_mut("plugged_in")
        .unwrap()
        .unknown("stale", 320.);
    tick(&mut e, 320.);
    feed(&mut e, 330., "vehicle/charging_state", "Disconnected");
    tick(&mut e, 330.);
    assert!(e.outputs[0].episodes[0].exhausted);
    assert!(!lights(&e)[0]);
}

#[test]
fn outages_do_not_extend_reminders_or_desynchronize_green_and_amber() {
    let mut e = example_engine();
    seed(&mut e, 0., true, "Disconnected");
    e.connection(false, 2.);
    assert!(tick(&mut e, 300.).is_empty());
    connect(&mut e, 305.);
    feed(&mut e, 305., "tele/garage-light/LWT", "Online");
    assert!(tick(&mut e, 305.).is_empty());
    tick(&mut e, 306.);
    assert_eq!(lights(&e), [false, true, true, false]);
    assert_eq!(e.outputs[1].episodes[0].deadline, Some(300.));
    assert_eq!(e.outputs[2].episodes[0].deadline, Some(300.));
}

#[test]
fn fault_restarts_only_on_admitted_edges_and_coalesces_simultaneous_events() {
    let mut e = example_engine();
    e.config.jitter.max_changes = 1;
    seed(&mut e, 0., true, "Charging");
    feed(&mut e, 10., "vehicle/fault", "low tyre pressure");
    tick(&mut e, 10.);
    position(&mut e, 20., 100.);
    tick(&mut e, 20.);
    position(&mut e, 21., 0.);
    tick(&mut e, 21.);
    assert_eq!(e.states[&State::LocationInner].value, Some(true));
    assert_eq!(
        e.controls[&State::LocationInner].admitted.value,
        Some(false)
    );
    assert_eq!(e.outputs[0].episodes[0].deadline, Some(310.));
    tick(&mut e, 30.);
    assert_eq!(e.outputs[0].episodes[0].deadline, Some(330.));

    let mut e = example_engine();
    seed(&mut e, 400., true, "Charging");
    feed(&mut e, 10., "vehicle/fault", "low tyre pressure");
    tick(&mut e, 10.);
    position(&mut e, 20., 0.);
    feed(&mut e, 20., "vehicle/charging_state", "Disconnected");
    tick(&mut e, 20.);
    assert_eq!(e.outputs[0].episodes[0].deadline, Some(320.));
    let value = snapshot(&e, 20.);
    let latest = value["state_history"]["entries"]
        .as_array()
        .unwrap()
        .last()
        .unwrap();
    let restarts: Vec<_> = latest["changes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["kind"] == "output_behavior_restarted" && c["output"] == "red")
        .collect();
    assert_eq!(restarts.len(), 1);
    assert_eq!(restarts[0]["triggers"].as_array().unwrap().len(), 2);
    assert_eq!(
        value["outputs"][0]["rules"][0]["restart_on"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn timeout_in_dry_run_records_off_without_calling_the_publish_sink() {
    let mut e = example_engine();
    e.dry = true;
    seed(&mut e, 0., true, "Charging");
    assert_eq!(e.outputs[2].simulated_value, Some(true));
    e.tick(600., |_| panic!("dry-run must not publish OFF either"));
    assert!(e
        .outputs
        .iter()
        .all(|o| o.simulated_value == Some(false) && o.submissions == 0));
    assert!(e.outputs.iter().all(|o| o.last_submitted.is_none()));
    assert!(snapshot(&e, 600.)["state_history"]
        .to_string()
        .contains("timeout"));
}

#[test]
fn telemetry_times_out_at_ten_minutes_and_only_valid_vehicle_data_recovers() {
    let mut e = example_engine();
    seed(&mut e, 0., true, "Charging");
    for (t, topic, payload) in [
        (500., "vehicle/battery_level", "NaN"),
        (550., "vehicle/charging_state", "unrecognized"),
        (590., "unknown/topic", "true"),
        (599., "tele/garage-light/LWT", "Online"),
    ] {
        feed(&mut e, t, topic, payload);
        tick(&mut e, t);
    }
    assert_eq!(e.next_deadline(599.5), 600.);
    assert_eq!(lights(&e), [false, false, true, true]);
    tick(&mut e, 600.);
    assert_eq!(lights(&e), [false; 4]);
    assert_eq!(e.telemetry_timed_out.value, Some(true));
    let entries = snapshot(&e, 600.)["state_history"]["entries"].clone();
    feed(&mut e, 601., "vehicle/location", "{}");
    feed(&mut e, 601., "tele/garage-light/LWT", "Online");
    assert!(tick(&mut e, 601.).is_empty());
    assert_eq!(snapshot(&e, 601.)["state_history"]["entries"], entries);
    feed(&mut e, 602., "vehicle/charging_state", "Charging");
    tick(&mut e, 602.);
    assert_eq!(lights(&e), [false, false, true, true]);
    let status = snapshot(&e, 602.);
    assert_eq!(status["telemetry"]["timed_out"]["value"], false);
    assert_eq!(status["telemetry"]["age_seconds"], 0.);
    assert_eq!(
        status["state_history"]["entries"].as_array().unwrap().len(),
        entries.as_array().unwrap().len() + 1
    );
    tick(&mut e, 1201.);
    assert_eq!(lights(&e), [false, false, true, true]);
    tick(&mut e, 1202.);
    assert_eq!(lights(&e), [false; 4]);
}

#[test]
fn timeout_supports_startup_no_data_disabled_mode_and_broker_recovery() {
    let mut e = example_engine();
    tick(&mut e, 599.);
    assert!(e.outputs.iter().all(|o| o.last_submitted.is_none()));
    tick(&mut e, 600.);
    assert_eq!(lights(&e), [false; 4]);

    let mut e = example_engine();
    e.config.telemetry_settings.timeout_seconds = None;
    seed(&mut e, 0., true, "Charging");
    tick(&mut e, 10000.);
    assert_eq!(lights(&e), [false, false, true, true]);

    let mut e = example_engine();
    seed(&mut e, 0., true, "Charging");
    e.connection(false, 590.);
    assert!(tick(&mut e, 600.).is_empty());
    assert!(e.outputs.iter().all(|o| o.desired.value == Some(false)));
    connect(&mut e, 610.);
    feed(&mut e, 610., "tele/garage-light/LWT", "Online");
    assert!(tick(&mut e, 610.).is_empty());
    tick(&mut e, 611.);
    assert_eq!(lights(&e), [false; 4]);
}

#[test]
fn configurable_timeout_suspends_blinking_without_restarting_its_deadline() {
    let mut e = example_engine();
    e.config.telemetry_settings.timeout_seconds = Some(10.);
    seed(&mut e, 0., true, "Disconnected");
    tick(&mut e, 10.);
    assert_eq!(lights(&e), [false; 4]);
    feed(&mut e, 12., "vehicle/charging_state", "Disconnected");
    tick(&mut e, 12.);
    assert_eq!(lights(&e), [false, false, true, false]);
    assert_eq!(e.outputs[2].episodes[0].deadline, Some(300.));
    assert_eq!(e.outputs[1].episodes[0].deadline, Some(300.));
}

#[test]
fn state_history_is_bounded_timestamped_and_explains_jitter() {
    let mut e = common::engine();
    for t in 1..=60 {
        feed(
            &mut e,
            t as f64,
            "car/locked",
            if t % 2 == 0 { "false" } else { "true" },
        );
        tick(&mut e, t as f64);
    }
    let value = snapshot(&e, 60.);
    let entries = value["state_history"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 50);
    assert_eq!(entries[0]["sequence"], 11);
    assert_eq!(entries[49]["sequence"], 60);
    for entry in entries {
        chrono::DateTime::parse_from_rfc3339(entry["timestamp"].as_str().unwrap()).unwrap();
        assert_eq!(entry["changes"].as_array().unwrap().len(), 1);
        assert_eq!(entry["changes"][0]["state"], "locked");
    }
    e.config.history_settings.max_entries = 2;
    feed(&mut e, 61., "car/locked", "true");
    tick(&mut e, 61.);
    assert_eq!(
        snapshot(&e, 61.)["state_history"]["entries"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    e.config.history_settings.max_entries = 0;
    feed(&mut e, 62., "car/locked", "false");
    tick(&mut e, 62.);
    assert_eq!(snapshot(&e, 62.)["state_history"]["entries"], json!([]));

    let mut e = common::engine();
    e.config.jitter.max_changes = 1;
    for (t, flag) in [(0., "false"), (1., "true"), (2., "false")] {
        feed(&mut e, t, "car/locked", flag);
        tick(&mut e, t);
    }
    let value = snapshot(&e, 2.);
    let pending = &value["state_history"]["entries"][2]["changes"][0];
    assert_eq!(pending["raw"], json!({"previous":true,"value":false}));
    assert_eq!(pending["control"], json!({"previous":true,"value":true}));
    tick(&mut e, 11.);
    let value = snapshot(&e, 11.);
    let admitted = &value["state_history"]["entries"][3]["changes"][0];
    assert_eq!(admitted["raw"], json!({"previous":false,"value":false}));
    assert_eq!(admitted["control"], json!({"previous":true,"value":false}));
}
