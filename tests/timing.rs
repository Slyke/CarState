mod common;
use carstate::{
    behaviors::Filter,
    config::JitterSettings,
    engine::{Engine, SubmissionError},
    model::State,
};
use common::*;
use serde_json::json;
#[test]
fn jitter_is_a_burst_then_full_cooldown_and_latest_value_wins() {
    let s = JitterSettings::default();
    let mut f = Filter::default();
    f.update(Some(false), -1., &s);
    for (t, v) in [(0., true), (1., false), (2., true)] {
        f.update(Some(v), t, &s);
        assert_eq!(f.admitted.value, Some(v));
    }
    assert_eq!(f.cooldown, Some(12.));
    f.update(Some(false), 3., &s);
    f.update(Some(true), 4., &s);
    f.update(Some(false), 11., &s);
    assert_eq!(f.admitted.value, Some(true));
    assert_eq!(f.cooldown, Some(12.));
    f.update(Some(false), 12., &s);
    assert_eq!(f.admitted.value, Some(false));
    assert_eq!(f.burst, 1);
    f.update(None, 13., &s);
    assert_eq!(f.admitted.value, None);
    assert_eq!(f.burst, 1);
    f.update(Some(false), 14., &s);
    assert_eq!(f.burst, 1);
    f.update(Some(true), 30., &s);
    assert_eq!(f.burst, 1);
}
#[test]
fn custom_jitter_budgets_and_same_value_at_deadline() {
    for count in [1, 5] {
        let settings = JitterSettings {
            max_changes: count,
            cooldown_seconds: 20.,
        };
        let mut f = Filter::default();
        f.update(Some(false), 0., &settings);
        for n in 1..=count {
            f.update(Some(n % 2 == 1), n as f64, &settings);
        }
        assert_eq!(f.cooldown, Some(count as f64 + 20.));
        let last = f.last_known;
        f.update(last, count as f64 + 20., &settings);
        assert_eq!(f.burst, 0);
        assert!(f.cooldown.is_none());
    }
}
#[test]
fn minimum_blink_starts_after_settling_and_completes_on_off_then_fallback() {
    let mut v = source();
    v["outputs"][1]["rules"][1]["behavior"] =
        json!({"macro":"blink_for","interval_seconds":2,"duration_seconds":2});
    let mut e = Engine::new(config(v), false);
    connect(&mut e, 0.);
    home(&mut e, 0., "Disconnected");
    assert!(tick(&mut e, 0.).is_empty());
    assert!(e.outputs[1].episodes[1].start.is_none());
    let first = tick(&mut e, 1.);
    assert!(first.contains(&("cmnd/test/POWER2".into(), "ON".into())));
    assert_eq!(e.outputs[1].episodes[1].start, Some(1.));
    assert!(tick(&mut e, 2.).contains(&("cmnd/test/POWER2".into(), "OFF".into())));
    assert!(tick(&mut e, 3.).contains(&("cmnd/test/POWER2".into(), "ON".into())));
    assert!(e.outputs[1].episodes[1].exhausted);
    for t in [4., 100., 86400., 172800.] {
        home(&mut e, t, "Disconnected");
        assert!(tick(&mut e, t).is_empty());
    }
}
#[test]
fn phases_skip_delayed_edges_and_preemption_does_not_extend_expiry() {
    let mut e = engine();
    home(&mut e, 0., "Disconnected");
    tick(&mut e, 1.);
    let start = e.outputs[1].episodes[1].start;
    tick(&mut e, 8.);
    assert_eq!(e.outputs[1].last_submitted, Some(false));
    feed(&mut e, 9., "car/fault", "true");
    tick(&mut e, 9.);
    assert_eq!(e.outputs[1].selected, Some(0));
    feed(&mut e, 10., "car/fault", "false");
    tick(&mut e, 10.);
    assert_eq!(e.outputs[1].episodes[1].start, start);
    feed(&mut e, 11., "car/fault", "true");
    tick(&mut e, 11.);
    tick(&mut e, 100.);
    assert!(e.outputs[1].episodes[1].exhausted);
    feed(&mut e, 101., "car/fault", "false");
    tick(&mut e, 101.);
    assert_eq!(e.outputs[1].selected, Some(2));
}
#[test]
fn unknown_suspends_without_rearming_and_only_admitted_false_rearms() {
    let mut e = engine();
    home(&mut e, 0., "Disconnected");
    tick(&mut e, 1.);
    tick(&mut e, 62.);
    assert!(e.outputs[1].episodes[1].exhausted);
    e.vehicle
        .booleans
        .get_mut("plugged_in")
        .unwrap()
        .unknown("stale", 63.);
    tick(&mut e, 63.);
    assert!(e.outputs[1].episodes[1].exhausted);
    feed(&mut e, 64., "car/status", "Disconnected");
    tick(&mut e, 64.);
    assert!(e.outputs[1].episodes[1].exhausted);
    feed(&mut e, 65., "car/status", "Complete");
    tick(&mut e, 65.);
    assert!(!e.outputs[1].episodes[1].exhausted);
    feed(&mut e, 66., "car/status", "Disconnected");
    tick(&mut e, 66.);
    assert_eq!(e.outputs[1].episodes[1].start, Some(66.));
}
#[test]
fn a_suppressed_false_trigger_cannot_rearm_an_exhausted_episode() {
    let mut e = engine();
    e.config.jitter = JitterSettings {
        max_changes: 1,
        cooldown_seconds: 100.,
    };
    home(&mut e, 0., "Charging");
    tick(&mut e, 1.);
    feed(&mut e, 2., "car/status", "Disconnected");
    tick(&mut e, 2.);
    tick(&mut e, 63.);
    assert!(e.outputs[1].episodes[1].exhausted);
    feed(&mut e, 64., "car/status", "Charging");
    tick(&mut e, 64.);
    feed(&mut e, 65., "car/status", "Disconnected");
    tick(&mut e, 65.);
    tick(&mut e, 103.);
    assert!(e.outputs[1].episodes[1].exhausted);
}
#[test]
fn retry_succeeds_without_input_and_replaces_obsolete_values() {
    let mut e = engine();
    home(&mut e, 0., "Charging");
    e.tick(1., |_| Err(SubmissionError::Rejected));
    assert!(!e.healthy());
    assert!(e.outputs.iter().all(|o| o.last_submitted_at.is_none()));
    assert!(tick(&mut e, 1.5).is_empty());
    tick(&mut e, 2.);
    assert!(e.healthy());
    feed(&mut e, 3., "car/fault", "true");
    e.tick(3., |_| Err(SubmissionError::Rejected));
    feed(&mut e, 3.5, "car/fault", "false");
    let commands = tick(&mut e, 4.);
    assert!(!commands
        .iter()
        .any(|(t, p)| t.ends_with("POWER1") && p == "ON"));
    assert!(e.healthy());
}
#[test]
fn every_submission_source_observes_hold_and_ambiguous_attempt_settling() {
    let mut e = engine();
    e.config.output_settings.min_hold_seconds = 2.;
    home(&mut e, 0., "Disconnected");
    connect(&mut e, 0.);
    let mut last = std::collections::BTreeMap::<String, f64>::new();
    for n in 0..200 {
        let t = n as f64 / 4.;
        if n == 40 {
            e.connection(false, t);
        }
        if n == 42 {
            connect(&mut e, t);
        }
        if n % 21 == 0 {
            feed(
                &mut e,
                t,
                "car/fault",
                if n % 42 == 0 { "true" } else { "false" },
            );
        }
        e.tick(t, |p| {
            if let Some(previous) = last.insert(p.topic.clone(), t) {
                assert!(t - previous >= 2., "too close: {previous} {t}");
            }
            Ok(())
        });
    }
    let mut e = engine();
    home(&mut e, 0., "Charging");
    e.tick(1., |_| Err(SubmissionError::Ambiguous));
    assert_eq!(e.outputs[0].eligible_at, 2.);
    assert!(tick(&mut e, 1.9).is_empty());
    assert!(!tick(&mut e, 2.).is_empty());
}
#[test]
fn outages_preserve_facts_deadlines_and_do_not_replay_missed_phases() {
    let mut e = engine();
    home(&mut e, 0., "Disconnected");
    tick(&mut e, 1.);
    let start = e.outputs[1].episodes[1].start;
    e.connection(false, 2.);
    assert!(tick(&mut e, 40.).is_empty());
    connect(&mut e, 80.);
    assert!(tick(&mut e, 80.).is_empty());
    tick(&mut e, 81.);
    assert_eq!(e.outputs[1].last_submitted, Some(true));
    assert_eq!(e.outputs[1].episodes[1].start, start);
    assert!(e.outputs[1].episodes[1].exhausted);
}
#[test]
fn device_availability_requires_a_new_session_report_and_duplicates_do_not_resync() {
    let mut v = source();
    v["output_devices"] = json!([{"name":"light","availability":{"topic":"lwt","true_values":["Online"],"false_values":["Offline"]}}]);
    for o in v["outputs"].as_array_mut().unwrap() {
        o["device"] = json!("light");
    }
    let mut e = Engine::new(config(v), false);
    connect(&mut e, 0.);
    home(&mut e, 0., "Charging");
    assert!(tick(&mut e, 2.).is_empty());
    assert!(!e.healthy());
    feed(&mut e, 3., "lwt", "Online");
    assert!(tick(&mut e, 3.).is_empty());
    assert_eq!(tick(&mut e, 4.).len(), 4);
    feed(&mut e, 5., "lwt", "Online");
    assert!(tick(&mut e, 5.).is_empty());
    e.connection(false, 6.);
    connect(&mut e, 7.);
    assert!(tick(&mut e, 8.).is_empty());
    feed(&mut e, 9., "lwt", "Online");
    assert_eq!(tick(&mut e, 10.).len(), 4);
}
#[test]
fn heartbeat_resyncs_before_pulse_and_its_own_failure_can_recover() {
    let mut e = engine();
    e.config.heartbeat.enabled = true;
    e.config.heartbeat.topic = Some("heartbeat".into());
    e.config.heartbeat.payload = Some("alive".into());
    home(&mut e, 0., "Charging");
    assert!(tick(&mut e, 0.).is_empty());
    let first = tick(&mut e, 1.);
    assert_eq!(first.len(), 5);
    assert_eq!(first.last().unwrap().0, "heartbeat");
    let mut qos = None;
    e.tick(121., |p| {
        if p.topic == "heartbeat" {
            qos = Some((p.qos, p.retain));
            Err(SubmissionError::Rejected)
        } else {
            Ok(())
        }
    });
    assert_eq!(qos, Some((0, false)));
    assert!(!e.healthy());
    assert!(e.heartbeat.failed);
    let retry = tick(&mut e, 122.);
    assert_eq!(retry.len(), 5);
    assert!(e.healthy());
    assert_eq!(e.heartbeat.submissions, 2);
    // Simulated watchdog fallback with no connection event: next cycle restores all known outputs.
    let restoration = tick(&mut e, 242.);
    assert_eq!(restoration.len(), 5);
    assert_eq!(e.outputs[2].last_submitted, Some(true));
    e.workers_healthy = false;
    assert!(tick(&mut e, 400.).is_empty());
    assert_eq!(e.heartbeat.submissions, 3);
}
#[test]
fn unknown_car_facts_do_not_stop_heartbeat_and_dry_run_never_invokes_sink() {
    let mut e = engine();
    e.dry = true;
    e.config.heartbeat.enabled = true;
    e.config.heartbeat.topic = Some("heartbeat".into());
    e.config.heartbeat.payload = Some("alive".into());
    e.tick(1., |_| panic!("dry-run published"));
    assert!(e.healthy());
    assert_eq!(e.heartbeat.simulated_submissions, 1);
    home(&mut e, 2., "Disconnected");
    for n in 2..200 {
        e.tick(n as f64, |_| panic!("dry-run published"));
    }
    assert!(e
        .outputs
        .iter()
        .all(|o| o.submissions == 0 && o.last_submitted_at.is_none()));
    assert!(e.outputs[1].simulated_submissions > 1);
    assert_eq!(e.heartbeat.submissions, 0);
    e.stopping = true;
    let before = e.outputs[1].simulated_submissions;
    e.tick(500., |_| panic!());
    assert_eq!(e.outputs[1].simulated_submissions, before);
}
#[test]
fn blink_phases_leave_trigger_timestamps_and_jitter_budget_unchanged() {
    let mut e = engine();
    home(&mut e, 0., "Disconnected");
    tick(&mut e, 1.);
    let state = State::LocationInnerParkedNotPluggedIn;
    let changed = e.states[&state].last_changed;
    for t in [3., 5., 7., 9.] {
        tick(&mut e, t);
        assert_eq!(e.states[&state].last_changed, changed);
        assert_eq!(e.controls[&state].burst, 0);
    }
}

#[test]
fn watchdog_simulator_covers_stopped_controller_broker_loss_and_device_restart() {
    // Models only the documented local RuleTimer contract; it does not verify installed firmware.
    struct Device {
        deadline: f64,
        relay: bool,
    }
    impl Device {
        fn boot(now: f64) -> Self {
            Self {
                deadline: now + 360.,
                relay: true,
            }
        }
        fn pulse(&mut self, now: f64) {
            self.deadline = now + 360.;
        }
        fn tick(&mut self, now: f64) {
            if now >= self.deadline {
                self.relay = false;
            }
        }
    }
    let mut d = Device::boot(0.);
    d.pulse(120.);
    d.tick(479.);
    assert!(d.relay);
    d.tick(480.);
    assert!(!d.relay);
    let mut restarted = Device::boot(500.);
    restarted.tick(860.);
    assert!(!restarted.relay); // no broker or controller pulse
    let mut e = engine();
    e.config.heartbeat.enabled = true;
    e.config.heartbeat.topic = Some("heartbeat".into());
    e.config.heartbeat.payload = Some("alive".into());
    home(&mut e, 0., "Charging");
    let updates = tick(&mut e, 1.);
    assert!(updates
        .iter()
        .any(|(t, p)| t.ends_with("POWER3") && p == "ON"));
    restarted.relay = true;
    restarted.pulse(861.);
    assert!(restarted.relay);
}

#[test]
fn hold_uses_actual_acceptance_time_instead_of_loop_start_time() {
    let mut e = engine();
    home(&mut e, 0., "Charging");
    let clock = std::cell::Cell::new(1.);
    e.tick_with_clock(
        1.,
        || clock.get(),
        |_| {
            clock.set(clock.get() + 0.01);
            Ok(())
        },
    );
    assert!((e.outputs[0].last_submitted_at.unwrap() - 1.01).abs() < 1e-9);
    for o in &mut e.outputs {
        o.force = true;
    }
    assert!(tick(&mut e, 2.).is_empty());
    assert_eq!(tick(&mut e, 2.05).len(), 4);
}
