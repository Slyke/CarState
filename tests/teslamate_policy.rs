mod common;

use carstate::{
    behaviors::Episode,
    config::{self, BooleanInput, LocationInput},
    engine::Engine,
    model::State,
};
use common::{connect, feed, tick};
use serde_json::json;
use std::collections::BTreeMap;

const VEHICLE_STATE: &str = "teslamate/cars/1/state";
const LOCATION: &str = "teslamate/cars/1/location";
const PLUGGED_IN: &str = "teslamate/cars/1/plugged_in";
const CHARGING_STATE: &str = "teslamate/cars/1/charging_state";

fn teslamate_engine() -> Engine {
    let (mut config, _, warnings) = config::parse(
        include_str!("../config/carstate.example.json5"),
        "{}",
        &BTreeMap::new(),
    )
    .unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    // Exercise the portable policy with the deployment's TeslaMate mappings.
    // No local configuration, credentials, broker, or physical relays are involved.
    config.inputs.location = Some(LocationInput::Json {
        topic: LOCATION.into(),
        stale_after_seconds: None,
    });
    config.inputs.parked = Some(BooleanInput {
        topic: VEHICLE_STATE.into(),
        true_values: ["online", "charging", "updating", "suspended", "asleep"]
            .map(String::from)
            .into(),
        false_values: vec!["driving".into()],
        stale_after_seconds: None,
    });
    config.inputs.online = Some(BooleanInput {
        topic: VEHICLE_STATE.into(),
        true_values: ["online", "charging", "driving", "updating"]
            .map(String::from)
            .into(),
        false_values: vec!["offline".into(), "asleep".into()],
        stale_after_seconds: None,
    });
    config.inputs.charging.as_mut().unwrap().topic = CHARGING_STATE.into();
    config.inputs.charge_complete.as_mut().unwrap().topic = CHARGING_STATE.into();
    config.inputs.plugged_in = Some(BooleanInput {
        topic: PLUGGED_IN.into(),
        true_values: vec!["true".into()],
        false_values: vec!["false".into()],
        stale_after_seconds: None,
    });
    // Keep the example's actual jitter and minimum-hold settings.
    config::validate(&config, &config::Secrets::default()).unwrap();
    let mut e = Engine::new(config, false);
    connect(&mut e, 0.);
    feed(&mut e, 0., "tele/garage-light/LWT", "Online");
    for (topic, payload) in [
        ("vehicle/fault", "healthy"),
        ("vehicle/tpms_soft_warning_fl", "false"),
        ("vehicle/battery_level", "80"),
        (PLUGGED_IN, "false"),
        (CHARGING_STATE, "Disconnected"),
    ] {
        feed(&mut e, 0., topic, payload);
    }
    e
}

fn position(e: &mut Engine, now: f64, meters: f64) {
    let home = &e.config.state_settings[0];
    let payload = json!({
        "latitude": home.target_latitude.unwrap() + (meters / 6_371_000.).to_degrees(),
        "longitude": home.target_longitude.unwrap(),
    });
    feed(e, now, LOCATION, &payload.to_string());
}

fn lights(e: &Engine) -> [bool; 4] {
    ["red", "orange", "green", "blue"].map(|name| {
        let i = e
            .config
            .outputs
            .iter()
            .position(|o| o.name == name)
            .unwrap();
        e.outputs[i].last_submitted.expect("known output")
    })
}

fn episode<'a>(e: &'a Engine, output: &str, rule: &str) -> &'a Episode {
    let i = e
        .config
        .outputs
        .iter()
        .position(|o| o.name == output)
        .unwrap();
    let runtime = &e.outputs[i];
    let j = runtime.rules.iter().position(|r| r.name == rule).unwrap();
    &runtime.episodes[j]
}

fn assert_reminder_deadline(e: &Engine, expected: Option<f64>) {
    assert_eq!(episode(e, "green", "plug_in_reminder").deadline, expected);
    assert_eq!(
        episode(e, "orange", "unplugged_reminder_delay").deadline,
        expected
    );
}

#[test]
fn driving_stays_orange_with_stale_home_gps_and_boundary_bounces() {
    let mut e = teslamate_engine();
    position(&mut e, 0., 0.);
    feed(&mut e, 0., VEHICLE_STATE, "online");
    feed(&mut e, 0., PLUGGED_IN, "true");
    feed(&mut e, 0., CHARGING_STATE, "Complete");
    tick(&mut e, 0.);
    tick(&mut e, 1.);
    assert_eq!(lights(&e), [false, false, true, false]);

    feed(&mut e, 10., PLUGGED_IN, "false");
    feed(&mut e, 10., CHARGING_STATE, "Disconnected");
    feed(&mut e, 10., VEHICLE_STATE, "driving");
    tick(&mut e, 10.);
    assert_eq!(lights(&e), [false, true, false, false]);
    assert_eq!(e.states[&State::LocationInner].value, Some(true));
    assert_reminder_deadline(&e, None);

    for (now, meters) in [(20., 45.), (30., 55.), (40., 45.), (70., 65.)] {
        position(&mut e, now, meters);
        tick(&mut e, now);
        assert_eq!(lights(&e), [false, true, false, false], "{meters}m");
        assert_reminder_deadline(&e, None);
    }
    position(&mut e, 80., 400.);
    tick(&mut e, 80.);
    assert_eq!(lights(&e), [true, false, false, false]);
}

#[test]
fn arrival_starts_a_full_five_minutes_after_both_home_and_park_are_known() {
    for parked_first in [false, true] {
        let mut e = teslamate_engine();
        position(&mut e, 0., 400.);
        feed(&mut e, 0., VEHICLE_STATE, "driving");
        tick(&mut e, 0.);
        tick(&mut e, 1.);

        if parked_first {
            feed(&mut e, 10., VEHICLE_STATE, "online");
        } else {
            position(&mut e, 10., 0.);
        }
        tick(&mut e, 10.);
        assert_reminder_deadline(&e, None);
        assert_eq!(
            lights(&e),
            if parked_first {
                [true, false, false, false]
            } else {
                [false, true, false, false]
            }
        );

        if parked_first {
            position(&mut e, 70., 0.);
        } else {
            feed(&mut e, 70., VEHICLE_STATE, "online");
        }
        tick(&mut e, 70.);
        assert_eq!(lights(&e), [false, false, true, false]);
        assert_reminder_deadline(&e, Some(370.));

        for now in 71..370 {
            if now == 120 {
                feed(&mut e, 120., VEHICLE_STATE, "online");
                feed(&mut e, 120., PLUGGED_IN, "false");
                feed(&mut e, 120., CHARGING_STATE, "Disconnected");
                position(&mut e, 120., 3.);
            }
            tick(&mut e, now as f64);
            assert_eq!(
                lights(&e),
                [false, false, ((now - 70) / 2) % 2 == 0, false],
                "parked_first={parked_first}, t={now}"
            );
        }
        tick(&mut e, 370.);
        assert_eq!(lights(&e), [false, true, true, false]);
        assert_reminder_deadline(&e, Some(370.));
        feed(&mut e, 380., VEHICLE_STATE, "online");
        feed(&mut e, 380., PLUGGED_IN, "false");
        tick(&mut e, 380.);
        assert_eq!(lights(&e), [false, true, true, false]);
        assert!(episode(&e, "green", "plug_in_reminder").exhausted);
        assert_reminder_deadline(&e, Some(370.));
    }
}

#[test]
fn plugging_in_cancels_the_reminder_even_when_charging_is_complete() {
    let mut e = teslamate_engine();
    position(&mut e, 0., 0.);
    feed(&mut e, 0., VEHICLE_STATE, "online");
    tick(&mut e, 0.);
    tick(&mut e, 1.);
    tick(&mut e, 2.);
    assert_eq!(lights(&e), [false, false, false, false]);
    assert_reminder_deadline(&e, Some(300.));

    // Charge completion does not supply the independent plug fact.
    feed(&mut e, 10., CHARGING_STATE, "Complete");
    tick(&mut e, 10.);
    assert_eq!(e.states[&State::PluggedIn].value, Some(false));
    assert_reminder_deadline(&e, Some(300.));
    feed(&mut e, 12., PLUGGED_IN, "true");
    tick(&mut e, 12.);
    assert_eq!(e.states[&State::Charging].value, Some(false));
    assert_eq!(e.states[&State::ChargeComplete].value, Some(true));
    assert_eq!(lights(&e), [false, false, true, false]);
    assert_reminder_deadline(&e, None);
    tick(&mut e, 14.);
    assert_eq!(lights(&e), [false, false, true, false]);

    feed(&mut e, 20., PLUGGED_IN, "false");
    tick(&mut e, 20.);
    assert_reminder_deadline(&e, Some(320.));
    tick(&mut e, 22.);
    assert_eq!(lights(&e), [false, false, false, false]);
}

#[test]
fn uncertain_vehicle_states_preserve_known_parked_and_its_reminder_deadline() {
    let mut e = teslamate_engine();
    position(&mut e, 0., 0.);
    feed(&mut e, 0., VEHICLE_STATE, "online");
    tick(&mut e, 0.);
    tick(&mut e, 1.);

    for (now, state) in [(10., "offline"), (14., "unavailable"), (18., "start")] {
        feed(&mut e, now, VEHICLE_STATE, state);
        tick(&mut e, now);
        assert_eq!(e.states[&State::Parked].value, Some(true));
        assert_eq!(lights(&e), [false, false, false, false]);
        assert_reminder_deadline(&e, Some(300.));
        tick(&mut e, now + 2.);
        assert_eq!(lights(&e), [false, false, true, false]);
    }
    tick(&mut e, 300.);
    assert_eq!(lights(&e), [false, true, true, false]);
    assert_reminder_deadline(&e, Some(300.));
}

#[test]
fn losing_vehicle_connectivity_during_departure_does_not_invent_parked() {
    let mut e = teslamate_engine();
    position(&mut e, 0., 0.);
    feed(&mut e, 0., VEHICLE_STATE, "driving");
    tick(&mut e, 0.);
    tick(&mut e, 1.);
    for (now, state) in [(10., "offline"), (20., "unavailable"), (30., "start")] {
        feed(&mut e, now, VEHICLE_STATE, state);
        tick(&mut e, now);
        assert_eq!(e.states[&State::Parked].value, Some(false));
        assert_eq!(lights(&e), [false, true, false, false]);
        assert_reminder_deadline(&e, None);
    }
}

#[test]
fn unknown_startup_waits_for_parked_telemetry_before_starting_a_reminder() {
    let mut e = teslamate_engine();
    position(&mut e, 0., 0.);
    tick(&mut e, 0.);
    tick(&mut e, 1.);
    for (now, state) in [(10., "offline"), (20., "unavailable"), (30., "start")] {
        feed(&mut e, now, VEHICLE_STATE, state);
        tick(&mut e, now);
        assert_eq!(e.states[&State::Parked].value, None);
        assert_eq!(
            e.states[&State::LocationInnerParkedNotPluggedIn].value,
            None
        );
        assert_reminder_deadline(&e, None);
        assert_eq!(e.outputs[2].last_submitted, Some(true));
    }
    feed(&mut e, 40., VEHICLE_STATE, "online");
    tick(&mut e, 40.);
    assert_eq!(e.states[&State::Parked].value, Some(true));
    assert_reminder_deadline(&e, Some(340.));
    tick(&mut e, 42.);
    assert_eq!(lights(&e), [false, false, false, false]);
}
