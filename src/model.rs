use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum State {
    LocationWithinOuter,
    LocationOuterBand,
    LocationInner,
    Charging,
    PluggedIn,
    ChargeComplete,
    Parked,
    Locked,
    BatteryLow,
    Online,
    SourceHealthy,
    VehicleFault,
    VehicleFaultFree,
    LocationInnerNotCharging,
    LocationInnerAndCharging,
    LocationInnerAndPluggedIn,
    LocationInnerNotPluggedIn,
    LocationInnerNotParked,
    LocationInnerParkedNotPluggedIn,
    LocationInnerPluggedInNotCharging,
    LocationInnerChargeComplete,
}
impl State {
    pub const ALL: [Self; 21] = [
        Self::LocationWithinOuter,
        Self::LocationOuterBand,
        Self::LocationInner,
        Self::Charging,
        Self::PluggedIn,
        Self::ChargeComplete,
        Self::Parked,
        Self::Locked,
        Self::BatteryLow,
        Self::Online,
        Self::SourceHealthy,
        Self::VehicleFault,
        Self::VehicleFaultFree,
        Self::LocationInnerNotCharging,
        Self::LocationInnerAndCharging,
        Self::LocationInnerAndPluggedIn,
        Self::LocationInnerNotPluggedIn,
        Self::LocationInnerNotParked,
        Self::LocationInnerParkedNotPluggedIn,
        Self::LocationInnerPluggedInNotCharging,
        Self::LocationInnerChargeComplete,
    ];
    pub fn name(self) -> String {
        serde_json::to_value(self)
            .expect("state enum")
            .as_str()
            .expect("state string")
            .into()
    }
}
pub type Truth = Option<bool>;
pub fn not(a: Truth) -> Truth {
    a.map(|v| !v)
}
pub fn and(values: impl IntoIterator<Item = Truth>) -> Truth {
    let mut unknown = false;
    for v in values {
        match v {
            Some(false) => return Some(false),
            None => unknown = true,
            _ => (),
        }
    }
    if unknown {
        None
    } else {
        Some(true)
    }
}
pub fn or(values: impl IntoIterator<Item = Truth>) -> Truth {
    let mut count = 0;
    let mut unknown = false;
    for v in values {
        count += 1;
        match v {
            Some(true) => return Some(true),
            None => unknown = true,
            _ => (),
        }
    }
    if unknown || count == 0 {
        None
    } else {
        Some(false)
    }
}
#[derive(Clone, Debug)]
pub struct Tracked<T> {
    pub value: Option<T>,
    pub last_changed: Option<f64>,
}
impl<T> Default for Tracked<T> {
    fn default() -> Self {
        Self {
            value: None,
            last_changed: None,
        }
    }
}
impl<T: PartialEq> Tracked<T> {
    pub fn set(&mut self, value: Option<T>, now: f64) -> bool {
        if self.value == value {
            return false;
        }
        self.value = value;
        self.last_changed = Some(now);
        true
    }
}
impl<T: Serialize> Tracked<T> {
    pub fn snapshot(&self, clock: &Clock) -> Value {
        json!({"value":self.value,"last_changed_at":clock.stamp(self.last_changed)})
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Location {
    pub latitude: f64,
    pub longitude: f64,
}
#[derive(Clone, Debug)]
pub struct Fact<T> {
    pub current: Tracked<T>,
    pub last_known: Option<T>,
    pub last_known_changed: Option<f64>,
    pub received: Option<f64>,
    pub stale_after: Option<f64>,
    pub configured: bool,
    pub reason: Option<&'static str>,
}
impl<T: Clone + PartialEq> Fact<T> {
    pub fn new(configured: bool, stale_after: Option<f64>) -> Self {
        Self {
            current: Tracked::default(),
            last_known: None,
            last_known_changed: None,
            received: None,
            stale_after,
            configured,
            reason: Some(if configured {
                "never_received"
            } else {
                "not_configured"
            }),
        }
    }
    pub fn receive(&mut self, value: T, receipt: f64, now: f64) {
        if self.last_known.as_ref() != Some(&value) {
            self.last_known_changed = Some(now);
        }
        self.last_known = Some(value.clone());
        self.received = Some(receipt);
        self.reason = None;
        self.current.set(Some(value), now);
    }
    pub fn unknown(&mut self, reason: &'static str, now: f64) {
        self.current.set(None, now);
        self.reason = Some(reason);
    }
    pub fn expire(&mut self, now: f64) {
        if self.deadline().is_some_and(|deadline| now >= deadline) && self.current.value.is_some() {
            self.unknown("stale", now);
        }
    }
    pub fn deadline(&self) -> Option<f64> {
        self.received.zip(self.stale_after).map(|(r, s)| r + s)
    }
}
impl<T: Clone + PartialEq + Serialize> Fact<T> {
    pub fn snapshot(&self, now: f64, clock: &Clock) -> Value {
        json!({"value":self.current.value,"last_changed_at":clock.stamp(self.current.last_changed),"last_received_at":clock.stamp(self.received),"freshness_deadline":clock.stamp(self.deadline()),"age_seconds":self.received.map(|r|(now-r).max(0.)),"unknown_reason":self.reason,"last_known_value":self.last_known,"last_known_changed_at":clock.stamp(self.last_known_changed)})
    }
}
#[derive(Clone)]
pub struct Clock {
    pub started: DateTime<Utc>,
    pub monotonic: tokio::time::Instant,
}
impl Default for Clock {
    fn default() -> Self {
        Self {
            started: Utc::now(),
            monotonic: tokio::time::Instant::now(),
        }
    }
}
impl Clock {
    pub fn stamp(&self, seconds: Option<f64>) -> Option<String> {
        seconds
            .and_then(|s| chrono::TimeDelta::try_milliseconds((s * 1000.) as i64))
            .and_then(|d| self.started.checked_add_signed(d))
            .map(|t| t.to_rfc3339_opts(SecondsFormat::Millis, true))
    }
}
pub type States = BTreeMap<State, Tracked<bool>>;
pub fn empty_states() -> States {
    State::ALL
        .into_iter()
        .map(|s| (s, Tracked::default()))
        .collect()
}
pub fn states_snapshot(states: &States, clock: &Clock) -> Value {
    Value::Object(
        states
            .iter()
            .map(|(s, v)| (s.name(), v.snapshot(clock)))
            .collect(),
    )
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildInfo {
    pub version: String,
    pub build_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_number: Option<String>,
}
impl Default for BuildInfo {
    fn default() -> Self {
        let v: Value =
            serde_json::from_str(include_str!(concat!(env!("OUT_DIR"), "/build-info.json")))
                .expect("generated build metadata");
        Self {
            version: env!("CARGO_PKG_VERSION").into(),
            build_hash: v["buildHash"].as_str().unwrap_or("unknown").into(),
            build_number: v["buildNumber"].as_str().map(str::to_owned),
        }
    }
}
