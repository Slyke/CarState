mod common;

use carstate::{
    engine::Engine,
    model::{Location, State},
    rules,
};
use common::{config, connect, feed, source, tick};
use serde_json::json;

fn controller(hysteresis: f64) -> Engine {
    let mut raw = source();
    raw["state_settings"][0]["hysteresis_meters"] = json!(hysteresis);
    let mut c = config(raw);
    c.jitter.max_changes = 1000;
    let mut e = Engine::new(c, false);
    connect(&mut e, 0.);
    feed(&mut e, 0., "car/parked", "true");
    feed(&mut e, 0., "car/status", "Disconnected");
    e
}

fn position(e: &mut Engine, now: f64, meters: f64) {
    let payload = json!({
        "latitude": (meters / 6_371_000.).to_degrees(),
        "longitude": 0.,
    });
    feed(e, now, "car/gps", &payload.to_string());
    tick(e, now);
}

fn membership(e: &Engine, inner: bool, within: bool) {
    assert_eq!(e.states[&State::LocationInner].value, Some(inner));
    assert_eq!(e.states[&State::LocationWithinOuter].value, Some(within));
    assert_eq!(
        e.states[&State::LocationOuterBand].value,
        Some(within && !inner)
    );
    assert_eq!(
        e.states[&State::LocationInnerParkedNotPluggedIn].value,
        Some(inner)
    );
    assert_eq!(
        e.states[&State::LocationInnerNotPluggedIn].value,
        Some(inner)
    );
}

#[test]
fn inner_exit_buffer_prevents_boundary_bounce_and_preserves_entry_radius() {
    let mut e = controller(20.);
    // The buffer cannot cause a fresh sample outside the entry radius to mean home.
    position(&mut e, 0., 60.);
    membership(&e, false, true);
    for (t, distance, inner) in [
        (1., 40., true),
        (2., 55., true),
        (3., 45., true),
        (4., 69., true),
        (5., 71., false),
        (6., 60., false),
        (7., 51., false),
        (8., 49., true),
    ] {
        position(&mut e, t, distance);
        membership(&e, inner, true);
    }
}

#[test]
fn outer_exit_buffer_keeps_band_and_within_membership_consistent() {
    let mut e = controller(20.);
    position(&mut e, 0., 310.);
    membership(&e, false, false);
    for (t, distance, within) in [
        (1., 290., true),
        (2., 310., true),
        (3., 299., true),
        (4., 319., true),
        (5., 321., false),
        (6., 310., false),
        (7., 301., false),
        (8., 299., true),
    ] {
        position(&mut e, t, distance);
        membership(&e, false, within);
    }
}

#[test]
fn zero_hysteresis_preserves_original_strict_inner_and_inclusive_outer_boundaries() {
    let mut e = controller(0.);
    let origin = Location {
        latitude: 0.,
        longitude: 0.,
    };
    let distance = |meters: f64| {
        rules::distance(
            origin,
            Location {
                latitude: (meters / 6_371_000.).to_degrees(),
                longitude: 0.,
            },
        )
    };
    // Use exactly the same calculated distances as the evaluator, avoiding
    // floating-point approximations at the configured thresholds.
    e.config.state_settings[0].inner_radius_meters = Some(distance(50.));
    e.config.state_settings[0].outer_radius_meters = Some(distance(300.));
    for (t, meters, inner, within) in [
        (0., 49., true, true),
        (1., 50., false, true),
        (2., 49., true, true),
        (3., 300., false, true),
        (4., 301., false, false),
        (5., 300., false, true),
    ] {
        position(&mut e, t, meters);
        membership(&e, inner, within);
    }
}

#[test]
fn memberships_are_independent_by_geofence_name() {
    let mut e = controller(20.);
    let mut second = e.config.state_settings[0].clone();
    second.name = "second".into();
    second.inner_radius_meters = Some(55.);
    second.hysteresis_meters = 0.;
    e.config.state_settings.push(second);

    position(&mut e, 0., 10.);
    position(&mut e, 1., 60.);
    assert_eq!(e.entries["home"][&State::LocationInner].value, Some(true));
    assert_eq!(
        e.entries["second"][&State::LocationInner].value,
        Some(false)
    );
    assert_eq!(e.states[&State::LocationInner].value, Some(true));
    // Combined states still OR the independently evaluated location entries.
    assert_eq!(e.states[&State::LocationOuterBand].value, Some(true));

    e.config.state_settings.reverse();
    position(&mut e, 2., 60.);
    assert_eq!(e.entries["home"][&State::LocationInner].value, Some(true));
    assert_eq!(
        e.entries["second"][&State::LocationInner].value,
        Some(false)
    );
    position(&mut e, 3., 80.);
    position(&mut e, 4., 60.);
    assert_eq!(e.entries["home"][&State::LocationInner].value, Some(false));
}

#[test]
fn unknown_location_resets_exit_memory_and_recovery_uses_entry_thresholds() {
    let mut e = controller(20.);
    e.vehicle.location.stale_after = Some(5.);
    position(&mut e, 0., 40.);
    position(&mut e, 1., 60.);
    membership(&e, true, true);
    feed(&mut e, 2., "car/gps", "{}");
    tick(&mut e, 2.);
    membership(&e, true, true);

    tick(&mut e, 6.);
    for state in [
        State::LocationInner,
        State::LocationWithinOuter,
        State::LocationOuterBand,
        State::LocationInnerParkedNotPluggedIn,
    ] {
        assert_eq!(e.states[&state].value, None);
        assert_eq!(e.entries["home"][&state].value, None);
    }
    position(&mut e, 7., 60.);
    membership(&e, false, true);

    position(&mut e, 8., 310.);
    membership(&e, false, true);
    tick(&mut e, 13.);
    position(&mut e, 14., 310.);
    membership(&e, false, false);
}
