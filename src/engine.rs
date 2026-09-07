//! Single-owner controller. All clocks are monotonic seconds injected by the caller.
use crate::{
    behaviors::Filter,
    config::{self, Config},
    inputs::{decode_bool, Vehicle},
    model::{self, BuildInfo, Clock, Fact, State, States, Tracked},
    outputs::OutputRuntime,
    rules,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};

#[derive(Clone, Debug)]
pub struct Publication {
    pub topic: String,
    pub payload: String,
    pub qos: u8,
    pub retain: bool,
}
#[derive(Clone, Copy, Debug)]
pub enum SubmissionError {
    Rejected,
    Ambiguous,
}
#[derive(Default)]
pub struct Heartbeat {
    pub next_due: f64,
    pub pass: bool,
    pub failed: bool,
    pub retry_count: u32,
    pub last_attempt: Option<f64>,
    pub last_submitted: Option<f64>,
    pub submissions: u64,
    pub simulated_at: Option<f64>,
    pub simulated_submissions: u64,
    pub last_error: Option<f64>,
    pub was_eligible: bool,
}
#[derive(Default)]
pub struct Counters {
    pub received: u64,
    pub rejected: u64,
    pub publish_failures: u64,
    pub reconnects: u64,
}
pub struct Engine {
    pub config: Config,
    pub vehicle: Vehicle,
    pub states: States,
    pub entries: BTreeMap<String, States>,
    pub controls: BTreeMap<State, Filter>,
    pub outputs: Vec<OutputRuntime>,
    pub devices: BTreeMap<String, Fact<bool>>,
    pub subscriptions: BTreeMap<String, Tracked<bool>>,
    pub connected: Tracked<bool>,
    pub generation: u64,
    pub heartbeat: Heartbeat,
    pub counters: Counters,
    pub last_received: Option<f64>,
    pub last_submitted: Option<f64>,
    pub last_error: Option<(&'static str, f64)>,
    pub workers_healthy: bool,
    pub stopping: bool,
    pub dry: bool,
    pub events: Vec<(&'static str, Value)>,
    pub telemetry_timed_out: Tracked<bool>,
    history: VecDeque<HistoryEntry>,
    history_sequence: u64,
    last_health: Option<bool>,
}
struct HistoryEntry {
    sequence: u64,
    at: f64,
    changes: Vec<Value>,
}
impl Engine {
    pub fn new(config: Config, dry: bool) -> Self {
        let vehicle = Vehicle::new(&config.inputs);
        let outputs = config
            .outputs
            .iter()
            .map(|o| OutputRuntime::new(o, config.output_settings.min_hold_seconds))
            .collect();
        let devices = config
            .output_devices
            .iter()
            .map(|d| {
                (
                    d.name.clone(),
                    Fact::new(true, d.availability.stale_after_seconds),
                )
            })
            .collect();
        let subscriptions = config::subscriptions(&config)
            .into_keys()
            .map(|t| {
                (
                    t,
                    Tracked {
                        value: Some(false),
                        last_changed: None,
                    },
                )
            })
            .collect();
        Self {
            config,
            vehicle,
            states: model::empty_states(),
            entries: BTreeMap::new(),
            controls: State::ALL
                .into_iter()
                .map(|s| (s, Filter::default()))
                .collect(),
            outputs,
            devices,
            subscriptions,
            connected: Tracked {
                value: Some(false),
                last_changed: None,
            },
            generation: 0,
            heartbeat: Heartbeat::default(),
            counters: Counters::default(),
            last_received: None,
            last_submitted: None,
            last_error: None,
            workers_healthy: true,
            stopping: false,
            dry,
            events: Vec::new(),
            telemetry_timed_out: Tracked {
                value: Some(false),
                last_changed: None,
            },
            history: VecDeque::new(),
            history_sequence: 0,
            last_health: None,
        }
    }
    pub fn connection(&mut self, connected: bool, now: f64) {
        if connected {
            self.generation += 1;
            if self.generation > 1 {
                self.counters.reconnects += 1;
            }
        }
        self.connected.set(Some(connected), now);
        for s in self.subscriptions.values_mut() {
            s.set(Some(false), now);
        }
        for d in self.devices.values_mut() {
            d.unknown("never_received", now);
        }
        for o in &mut self.outputs {
            o.resync(
                now,
                self.config.output_settings.min_hold_seconds,
                true,
                "broker_recovery",
            );
        }
        self.heartbeat.pass = false;
        self.heartbeat.was_eligible = false;
        self.heartbeat.next_due = now;
        if !connected {
            self.last_error = Some(("mqtt_disconnected", now));
        }
    }
    pub fn acknowledged(&mut self, topics: &[String], granted: &[bool], now: f64) {
        for (i, t) in topics.iter().enumerate() {
            if let Some(s) = self.subscriptions.get_mut(t) {
                s.set(Some(granted.get(i) == Some(&true)), now);
            }
        }
    }
    pub fn receive(&mut self, topic: &str, payload: &[u8], now: f64) {
        self.counters.received += 1;
        self.last_received = Some(now);
        let mut rejected = self
            .vehicle
            .receive(&self.config.inputs, topic, payload, now);
        for d in &self.config.output_devices {
            if d.availability.topic != topic {
                continue;
            }
            let fact = self.devices.get_mut(&d.name).expect("configured device");
            if let Some(v) = decode_bool(&d.availability, payload) {
                let recovered = v && fact.current.value != Some(true);
                let changed = fact.current.value != Some(v);
                fact.receive(v, now, now);
                if changed {
                    self.events.push((
                        "DEVICE_AVAILABILITY_CHANGED",
                        json!({"device":d.name,"online":v}),
                    ));
                }
                if recovered {
                    for (i, o) in self.config.outputs.iter().enumerate() {
                        if o.device.as_ref() == Some(&d.name) {
                            self.outputs[i].resync(
                                now,
                                self.config.output_settings.min_hold_seconds,
                                true,
                                "device_recovery",
                            );
                        }
                    }
                }
            } else {
                rejected = true;
            }
        }
        if !rejected && self.subscriptions.contains_key(topic) {
            self.events.push(("INPUT_RECEIVED", json!({"topic":topic})));
        }
        if rejected {
            self.counters.rejected += 1;
            self.events.push(("INPUT_REJECTED", json!({"topic":topic})));
        }
    }
    pub fn transport_ready(&self) -> bool {
        self.connected.value == Some(true)
            && self.subscriptions.values().all(|s| s.value == Some(true))
    }
    pub fn devices_ready(&self) -> bool {
        self.devices.values().all(|d| d.current.value == Some(true))
    }
    pub fn healthy_without_heartbeat(&self) -> bool {
        !self.stopping
            && self.workers_healthy
            && self.transport_ready()
            && self.devices_ready()
            && self.outputs.iter().all(|o| !o.unresolved)
    }
    pub fn healthy(&self) -> bool {
        self.healthy_without_heartbeat() && !self.heartbeat.failed
    }
    fn available(&self, index: usize) -> bool {
        !self.stopping
            && self.workers_healthy
            && self.transport_ready()
            && self.config.outputs[index]
                .device
                .as_ref()
                .is_none_or(|d| self.devices[d].current.value == Some(true))
    }
    /// Non-blocking sink acceptance. A recording sink can inject rejection/ambiguous failures.
    /// Dry-run never calls the sink, including for heartbeat decisions.
    pub fn tick(
        &mut self,
        now: f64,
        submit: impl FnMut(&Publication) -> Result<(), SubmissionError>,
    ) {
        self.tick_with_clock(now, || now, submit);
    }
    /// Runtime acceptance timestamps are sampled after each submission. This keeps
    /// the hold conservative even when evaluating many outputs takes measurable time.
    pub fn tick_with_clock(
        &mut self,
        now: f64,
        mut clock: impl FnMut() -> f64,
        mut submit: impl FnMut(&Publication) -> Result<(), SubmissionError>,
    ) {
        let mut changes = Vec::new();
        let timed_out = self
            .telemetry_deadline()
            .is_some_and(|deadline| now >= deadline);
        let previous = self.telemetry_timed_out.value;
        if self.telemetry_timed_out.set(Some(timed_out), now) {
            let context = json!({"kind":"telemetry","previous":previous,"value":timed_out,"reason":if timed_out {"timeout"} else {"valid_telemetry_received"}});
            changes.push(context.clone());
            self.events.push((
                if timed_out {
                    "TELEMETRY_TIMED_OUT"
                } else {
                    "TELEMETRY_RECOVERED"
                },
                context,
            ));
        }
        let before = self.freshness_status();
        self.vehicle.expire(now);
        for d in self.devices.values_mut() {
            d.expire(now);
        }
        for (name, reason) in self.freshness_status() {
            if before.get(&name) != Some(&reason) {
                self.events.push((
                    "INPUT_FRESHNESS_CHANGED",
                    json!({"input":name,"unknown_reason":reason}),
                ));
            }
        }
        let (raw, entries) = rules::evaluate(&self.config, &self.vehicle);
        let mut admitted_changes = BTreeMap::new();
        for (s, v) in raw {
            let previous_raw = self.states[&s].value;
            if self.states.get_mut(&s).expect("state").set(v, now) {
                self.events.push((
                    "STATE_CHANGED",
                    json!({"state":s,"previous":previous_raw,"value":v}),
                ));
            }
            let filter = self.controls.get_mut(&s).expect("filter");
            let before = filter.admitted.value;
            let cooldown = filter.cooldown;
            filter.update(v, now, &self.config.jitter);
            if before != filter.admitted.value {
                admitted_changes.insert(s, (before, filter.admitted.value));
            }
            if previous_raw != v || before != filter.admitted.value {
                changes.push(json!({"kind":"state","state":s,"raw":{"previous":previous_raw,"value":v},"control":{"previous":before,"value":filter.admitted.value}}));
            }
            if before != filter.admitted.value || cooldown != filter.cooldown {
                self.events.push(("CONTROL_STATE_ADMITTED",json!({"state":s,"value":filter.admitted.value,"cooldown_deadline_seconds":filter.cooldown,"burst_count":filter.burst})));
            }
        }
        for (name, values) in entries {
            let e = self.entries.entry(name.clone()).or_default();
            for (s, v) in values {
                let state = e.entry(s).or_default();
                let previous = state.value;
                if state.set(v, now) {
                    changes.push(json!({"kind":"entry_state","entry":name,"state":s,"previous":previous,"value":v}));
                }
            }
        }
        if self.stopping {
            self.record_history(now, changes);
            return;
        }
        let eligible = self.transport_ready() && self.devices_ready() && self.workers_healthy;
        if self.config.heartbeat.enabled && eligible && !self.heartbeat.was_eligible {
            self.heartbeat.next_due = self.heartbeat.next_due.min(now);
        }
        self.heartbeat.was_eligible = eligible;
        if self.config.heartbeat.enabled
            && eligible
            && now >= self.heartbeat.next_due
            && !self.heartbeat.pass
        {
            self.heartbeat.pass = true;
            for o in &mut self.outputs {
                o.resync(
                    now,
                    self.config.output_settings.min_hold_seconds,
                    false,
                    "heartbeat_resync",
                );
            }
        }
        for i in 0..self.outputs.len() {
            let now = clock();
            let available = self.available(i);
            let o = &self.config.outputs[i];
            let runtime = &mut self.outputs[i];
            let mut events = runtime.restart_on_transitions(&admitted_changes, &self.controls, now);
            events.extend(runtime.select(o, &self.controls, now, available, self.dry, timed_out));
            for (key, mut context) in events {
                context["output"] = json!(o.name);
                if key == "OUTPUT_RULE_SELECTED" || key.starts_with("OUTPUT_BEHAVIOR_") {
                    let mut change = context.clone();
                    change["kind"] = json!(key.to_ascii_lowercase());
                    changes.push(change);
                }
                self.events.push((key, context));
            }
            if !available {
                continue;
            }
            let Some(value) = runtime.due(now, self.dry) else {
                continue;
            };
            let publication = Publication {
                topic: o.topic.clone(),
                payload: o.payload(value).into(),
                qos: self.config.mqtt_settings.qos,
                retain: self.config.mqtt_settings.retain_commands,
            };
            let outcome = if self.dry {
                Ok(())
            } else {
                submit(&publication)
            };
            let now = clock();
            match outcome {
                Ok(()) => {
                    let recovered = runtime.unresolved;
                    runtime.accepted(
                        value,
                        now,
                        self.config.output_settings.min_hold_seconds,
                        self.dry,
                    );
                    if !self.dry {
                        self.last_submitted = Some(now);
                    }
                    self.events.push((if self.dry {"WOULD_PUBLISH"} else if recovered {"OUTPUT_SUBMISSION_RECOVERED"} else {"OUTPUT_BLINK_PHASE"},json!({"output":o.name,"topic":o.topic,"value":value,"qos":publication.qos,"retain":publication.retain,"simulated":self.dry,"payload":publication.payload})));
                }
                Err(error) => {
                    runtime.failed(
                        now,
                        &self.config,
                        matches!(error, SubmissionError::Ambiguous),
                    );
                    self.counters.publish_failures += 1;
                    self.last_error = Some(("publish_failed", now));
                    self.events.push((
                        "OUTPUT_SUBMISSION_FAILED",
                        json!({"output":o.name,"retry_count":runtime.retry_count}),
                    ));
                }
            }
        }
        let now = clock();
        if self.config.heartbeat.enabled
            && self.heartbeat.pass
            && now >= self.heartbeat.next_due
            && self.healthy_without_heartbeat()
            && self.outputs.iter().all(|o| !o.pending(self.dry))
        {
            self.heartbeat.last_attempt = Some(now);
            let h = &self.config.heartbeat;
            let publication = Publication {
                topic: h.topic.clone().expect("validated heartbeat topic"),
                payload: h.payload.clone().expect("validated heartbeat payload"),
                qos: 0,
                retain: false,
            };
            let result = if self.dry {
                Ok(())
            } else {
                submit(&publication)
            };
            let now = clock();
            self.heartbeat.pass = false;
            match result {
                Ok(()) => {
                    if self.dry {
                        self.heartbeat.simulated_at = Some(now);
                        self.heartbeat.simulated_submissions += 1;
                    } else {
                        self.heartbeat.last_submitted = Some(now);
                        self.heartbeat.submissions += 1;
                        self.last_submitted = Some(now);
                    }
                    let recovered = self.heartbeat.failed;
                    self.heartbeat.failed = false;
                    self.heartbeat.retry_count = 0;
                    self.heartbeat.next_due = now + h.interval_seconds;
                    self.events.push((
                        if self.dry {
                            "WOULD_HEARTBEAT"
                        } else if recovered {
                            "HEARTBEAT_RECOVERED"
                        } else {
                            "HEARTBEAT_SUBMITTED"
                        },
                        json!({"simulated":self.dry,"payload":publication.payload}),
                    ));
                }
                Err(_) => {
                    self.heartbeat.failed = true;
                    self.heartbeat.last_error = Some(now);
                    self.heartbeat.retry_count = self.heartbeat.retry_count.saturating_add(1);
                    self.heartbeat.next_due = now
                        + (self.config.mqtt_settings.publish_retry_initial_seconds
                            * 2_f64.powi(self.heartbeat.retry_count.min(32) as i32 - 1))
                        .min(self.config.mqtt_settings.publish_retry_max_seconds);
                    self.counters.publish_failures += 1;
                    self.events.push((
                        "HEARTBEAT_SUBMISSION_FAILED",
                        json!({"retry_count":self.heartbeat.retry_count}),
                    ));
                }
            }
        }
        let healthy = self.healthy();
        if self.last_health != Some(healthy) {
            self.last_health = Some(healthy);
            self.events.push((
                "CONTROL_PIPELINE_HEALTH_CHANGED",
                json!({"healthy":healthy}),
            ));
            if self.config.heartbeat.enabled {
                self.events.push((
                    if healthy {
                        "HEARTBEAT_AUTHORIZATION_RECOVERED"
                    } else {
                        "HEARTBEAT_SUPPRESSED"
                    },
                    json!({"control_pipeline_healthy":healthy}),
                ));
            }
        }
        self.record_history(now, changes);
    }
    fn record_history(&mut self, now: f64, changes: Vec<Value>) {
        if changes.is_empty() {
            return;
        }
        self.history_sequence += 1;
        self.events.push((
            "STATE_TRANSITION",
            json!({"sequence":self.history_sequence,"uptime_seconds":now,"changes":changes}),
        ));
        let limit = self.config.history_settings.max_entries;
        if limit == 0 {
            self.history.clear();
            return;
        }
        while self.history.len() >= limit {
            self.history.pop_front();
        }
        self.history.push_back(HistoryEntry {
            sequence: self.history_sequence,
            at: now,
            changes,
        });
    }
    fn telemetry_deadline(&self) -> Option<f64> {
        self.config
            .telemetry_settings
            .timeout_seconds
            .map(|timeout| self.vehicle.last_received.unwrap_or(0.) + timeout)
    }
    fn freshness_status(&self) -> BTreeMap<String, Option<&'static str>> {
        let mut result: BTreeMap<_, _> = self
            .vehicle
            .booleans
            .iter()
            .map(|(n, f)| (n.clone(), f.reason))
            .collect();
        result.insert("location".into(), self.vehicle.location.reason);
        result.insert("battery_percent".into(), self.vehicle.battery.reason);
        for (n, f) in &self.vehicle.faults {
            result.insert(format!("fault:{n}"), f.reason);
        }
        for (n, f) in &self.devices {
            result.insert(format!("device:{n}"), f.reason);
        }
        result
    }
    /// Next meaningful deadline, capped at one second for idle-capable worker supervision.
    pub fn next_deadline(&self, now: f64) -> f64 {
        let mut next = now + 1.;
        let mut add = |t: Option<f64>| {
            if let Some(t) = t {
                if t > now {
                    next = next.min(t);
                }
            }
        };
        for f in self
            .vehicle
            .booleans
            .values()
            .chain(self.vehicle.faults.values())
        {
            add(f.deadline());
        }
        add(self.vehicle.battery.deadline());
        add(self.vehicle.location.deadline());
        add(self.vehicle.split.assembly_deadline);
        add(self.telemetry_deadline());
        for f in self.devices.values() {
            add(f.deadline());
        }
        for f in self.controls.values() {
            add(f.cooldown);
        }
        for (i, o) in self.outputs.iter().enumerate() {
            if o.pending(self.dry) && self.available(i) {
                add(Some(o.eligible_at));
                add(o.retry_at);
            }
            for (j, e) in o.episodes.iter().enumerate() {
                add(e.deadline);
                if o.selected == Some(j) {
                    if let config::Behavior::BlinkFor {
                        interval_seconds, ..
                    } = o.rules[j].behavior
                    {
                        add(e.next_phase(now, interval_seconds));
                    }
                }
            }
        }
        if self.config.heartbeat.enabled {
            add(Some(self.heartbeat.next_due));
        }
        next
    }
    pub fn snapshot(&self, now: f64, clock: &Clock, identity: &SnapshotIdentity) -> Snapshot {
        let c = &self.config;
        let roles = config::subscriptions(c);
        Snapshot {
            service: "carstate",
            build: identity.build.clone(),
            generated_at: clock.stamp(Some(now)),
            runtime: json!({"started_at":clock.stamp(Some(0.)),"uptime_seconds":now,"mode":if self.dry {"dry_run"} else {"normal"},"publishing_enabled":!self.dry,"ready":self.healthy(),"control_pipeline_healthy":self.healthy(),"workers":{"evaluator":{"healthy":self.workers_healthy},"timer_scheduler":{"healthy":self.workers_healthy},"publisher":{"healthy":self.workers_healthy},"mqtt":{"healthy":self.workers_healthy},"last_progress_at":clock.stamp(Some(now)),"expected_wait":"bounded MQTT I/O or scheduled deadline"},"http":c.http,"single_writer_enforcement":"operational_assumption"}),
            mqtt: json!({"client_id":identity.client_id,"client_id_generated":identity.generated,"broker":{"host":c.mqtt_settings.ip,"port":c.mqtt_settings.port,"use_tls":c.mqtt_settings.use_tls,"validate_certs":c.mqtt_settings.validate_certs},"connected":self.connected.snapshot(clock),"connection_generation":self.generation,"qos":c.mqtt_settings.qos,"retain_commands":c.mqtt_settings.retain_commands,"credentials_configured":identity.credentials,"subscriptions":self.subscriptions.iter().map(|(t,s)|json!({"topic":t,"roles":roles[t],"qos":c.mqtt_settings.qos,"active":s.snapshot(clock)})).collect::<Vec<_>>(),"last_received_at":clock.stamp(self.last_received),"last_submitted_at":clock.stamp(self.last_submitted),"last_error":self.last_error.map(|(code,t)|json!({"code":code,"at":clock.stamp(Some(t))})),"counters":{"received_messages":self.counters.received,"rejected_messages":self.counters.rejected,"publish_submission_failures":self.counters.publish_failures,"successful_reconnects":self.counters.reconnects}}),
            facts: self.vehicle.snapshot(now, clock),
            telemetry: json!({"timeout_seconds":c.telemetry_settings.timeout_seconds,"last_valid_received_at":clock.stamp(self.vehicle.last_received),"age_seconds":self.vehicle.last_received.map(|t|now-t),"timeout_at":clock.stamp(self.telemetry_deadline()),"timed_out":self.telemetry_timed_out.snapshot(clock),"all_lights_off_requested":self.telemetry_timed_out.value == Some(true)}),
            state_history: json!({"max_entries":c.history_settings.max_entries,"order":"oldest_first","entries":self.history.iter().map(|e|json!({"sequence":e.sequence,"timestamp":clock.stamp(Some(e.at)),"uptime_seconds":e.at,"changes":e.changes})).collect::<Vec<_>>()}),
            states: model::states_snapshot(&self.states, clock),
            control_states: Value::Object(
                self.controls
                    .iter()
                    .map(|(s, f)| (s.name(), f.snapshot(clock, &c.jitter)))
                    .collect(),
            ),
            entry_states: Value::Object(
                self.entries
                    .iter()
                    .map(|(n, s)| (n.clone(), model::states_snapshot(s, clock)))
                    .collect(),
            ),
            devices: Value::Object(
                self.devices
                    .iter()
                    .map(|(n, f)| {
                        let mut v = f.snapshot(now, clock);
                        v["resynchronization_pending"] =
                            json!(c.outputs.iter().enumerate().any(|(i, o)| o.device.as_ref()
                                == Some(n)
                                && self.outputs[i].force));
                        (n.clone(), v)
                    })
                    .collect(),
            ),
            heartbeat: json!({"enabled":c.heartbeat.enabled,"suppressed":c.heartbeat.enabled && !self.healthy(),"suppression_reason":if !c.heartbeat.enabled {Some("disabled")} else if !self.healthy_without_heartbeat() {Some("control_pipeline_unavailable")} else if self.heartbeat.failed {Some("heartbeat_submission_failed")} else {None},"topic":c.heartbeat.topic,"interval_seconds":c.heartbeat.interval_seconds,"expected_timeout_seconds":c.heartbeat.timeout_seconds,"last_attempt_at":clock.stamp(self.heartbeat.last_attempt),"last_submitted_at":clock.stamp(self.heartbeat.last_submitted),"next_due_at":clock.stamp(Some(self.heartbeat.next_due)),"pending_resynchronization":self.heartbeat.pass,"submission_count":self.heartbeat.submissions,"last_error":self.heartbeat.last_error.map(|t|json!({"code":"heartbeat_submission_failed","at":clock.stamp(Some(t))})),"simulated_submissions":self.heartbeat.simulated_submissions,"simulated_at":clock.stamp(self.heartbeat.simulated_at),"watchdog_verification":"not_verified"}),
            outputs: self
                .outputs
                .iter()
                .zip(&c.outputs)
                .map(|(r, o)| r.snapshot(o, c, &self.controls, now, clock, self.dry))
                .collect(),
        }
    }
}
pub struct SnapshotIdentity {
    pub build: BuildInfo,
    pub client_id: String,
    pub generated: bool,
    pub credentials: bool,
}
/// Only explicitly constructed diagnostic fields are serializable. Secrets/config are never embedded.
#[derive(Clone, Serialize)]
pub struct Snapshot {
    pub service: &'static str,
    #[serde(flatten)]
    pub build: BuildInfo,
    pub generated_at: Option<String>,
    pub runtime: Value,
    pub mqtt: Value,
    pub facts: Value,
    pub telemetry: Value,
    pub state_history: Value,
    pub states: Value,
    pub control_states: Value,
    pub entry_states: Value,
    pub devices: Value,
    pub heartbeat: Value,
    pub outputs: Vec<Value>,
}
