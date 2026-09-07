use crate::{
    config::{BooleanInput, Inputs, LocationInput},
    model::{Clock, Fact, Location},
};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub struct Vehicle {
    pub booleans: BTreeMap<String, Fact<bool>>,
    pub faults: BTreeMap<String, Fact<bool>>,
    pub location: Fact<Location>,
    pub battery: Fact<f64>,
    pub split: SplitLocation,
}
#[derive(Default)]
pub struct SplitLocation {
    pub latitude: Option<(f64, f64, u64)>,
    pub longitude: Option<(f64, f64, u64)>,
    pub generation: u64,
    pub committed: u64,
    pub assembly_deadline: Option<f64>,
}
impl Vehicle {
    pub fn new(inputs: &Inputs) -> Self {
        let mut booleans = [
            "charging",
            "plugged_in",
            "charge_complete",
            "parked",
            "locked",
            "online",
            "source_healthy",
        ]
        .into_iter()
        .map(|n| (n.into(), Fact::new(false, None)))
        .collect::<BTreeMap<_, _>>();
        for (n, m) in inputs.booleans() {
            booleans.insert(n.into(), Fact::new(true, m.stale_after_seconds));
        }
        Self {
            booleans,
            faults: inputs
                .faults
                .iter()
                .map(|f| {
                    (
                        f.name.clone(),
                        Fact::new(true, f.mapping.stale_after_seconds),
                    )
                })
                .collect(),
            location: Fact::new(
                inputs.location.is_some(),
                inputs.location.as_ref().and_then(LocationInput::stale),
            ),
            battery: Fact::new(
                inputs.battery.is_some(),
                inputs.battery.as_ref().and_then(|b| b.stale_after_seconds),
            ),
            split: SplitLocation::default(),
        }
    }
    pub fn boolean(&self, name: &str) -> Option<bool> {
        self.booleans.get(name).and_then(|f| f.current.value)
    }
    /// Apply all recognized decoders before the caller performs exactly one evaluation.
    /// Returns whether any configured decoder rejected the payload, without exposing it.
    pub fn receive(&mut self, inputs: &Inputs, topic: &str, payload: &[u8], now: f64) -> bool {
        let mut rejected = false;
        for (name, m) in inputs.booleans() {
            if m.topic == topic {
                if let Some(v) = decode_bool(m, payload) {
                    self.booleans
                        .get_mut(name)
                        .expect("configured fact")
                        .receive(v, now, now);
                } else {
                    rejected = true;
                }
            }
        }
        for f in &inputs.faults {
            if f.mapping.topic == topic {
                if let Some(v) = decode_bool(&f.mapping, payload) {
                    self.faults
                        .get_mut(&f.name)
                        .expect("configured fault")
                        .receive(v, now, now);
                } else {
                    rejected = true;
                }
            }
        }
        if inputs.battery.as_ref().is_some_and(|m| m.topic == topic) {
            if let Some(v) = number(payload, 0., 100.) {
                self.battery.receive(v, now, now);
            } else {
                rejected = true;
            }
        }
        if let Some(location) = &inputs.location {
            match location {
                LocationInput::Json { topic: t, .. } if t == topic => {
                    if let Some(v) = decode_location(payload) {
                        self.location.receive(v, now, now);
                    } else {
                        rejected = true;
                    }
                }
                LocationInput::Split {
                    latitude_topic,
                    longitude_topic,
                    max_coordinate_skew_seconds,
                    ..
                } if latitude_topic == topic || longitude_topic == topic => {
                    let latitude = latitude_topic == topic;
                    let range = if latitude { 90. } else { 180. };
                    if let Some(v) = number(payload, -range, range) {
                        self.split.generation += 1;
                        let component = Some((v, now, self.split.generation));
                        if latitude {
                            self.split.latitude = component;
                        } else {
                            self.split.longitude = component;
                        }
                        self.split
                            .assembly_deadline
                            .get_or_insert(now + max_coordinate_skew_seconds);
                        self.assemble(*max_coordinate_skew_seconds, now);
                    } else {
                        rejected = true;
                    }
                }
                _ => (),
            }
        }
        rejected
    }
    fn assemble(&mut self, skew: f64, now: f64) {
        if let (Some((lat, lt, lg)), Some((lon, ot, og))) =
            (self.split.latitude, self.split.longitude)
        {
            let fresh = self
                .location
                .stale_after
                .is_none_or(|s| now - lt < s && now - ot < s);
            if lg > self.split.committed
                && og > self.split.committed
                && (lt - ot).abs() <= skew
                && fresh
            {
                self.location.receive(
                    Location {
                        latitude: lat,
                        longitude: lon,
                    },
                    lt.min(ot),
                    now,
                );
                self.split.committed = self.split.generation;
                self.split.assembly_deadline = None;
            } else if (lt - ot).abs() > skew || !fresh {
                // Remove obsolete candidates; retain the newest axis for a late matching peer.
                if lt <= ot {
                    self.split.latitude = None;
                } else {
                    self.split.longitude = None;
                }
            }
        }
    }
    pub fn expire(&mut self, now: f64) {
        for fact in self.booleans.values_mut().chain(self.faults.values_mut()) {
            fact.expire(now);
        }
        self.battery.expire(now);
        self.location.expire(now);
        if self.split.assembly_deadline.is_some_and(|d| now >= d) {
            self.location.unknown("incoherent", now);
        }
    }
    pub fn snapshot(&self, now: f64, clock: &Clock) -> Value {
        let mut result = serde_json::Map::new();
        for (name, fact) in &self.booleans {
            result.insert(name.clone(), fact.snapshot(now, clock));
        }
        result.insert("battery_percent".into(), self.battery.snapshot(now, clock));
        let mut location = self.location.snapshot(now, clock);
        let component = |v: Option<(f64, f64, u64)>| {
            v.map(|(value,t,g)|json!({"value":value,"last_received_at":clock.stamp(Some(t)),"generation":g,"pending":g>self.split.committed}))
        };
        location["split"] = json!({"latitude":component(self.split.latitude),"longitude":component(self.split.longitude),"committed_generation":self.split.committed,"assembly_deadline":clock.stamp(self.split.assembly_deadline),"pair_pending":self.split.assembly_deadline.is_some()});
        result.insert("location".into(), location);
        result.insert(
            "faults".into(),
            Value::Object(
                self.faults
                    .iter()
                    .map(|(n, f)| (n.clone(), f.snapshot(now, clock)))
                    .collect(),
            ),
        );
        result.into()
    }
}
pub fn decode_bool(mapping: &BooleanInput, bytes: &[u8]) -> Option<bool> {
    let text = std::str::from_utf8(bytes).ok()?.trim();
    if mapping
        .true_values
        .iter()
        .any(|v| v.trim().eq_ignore_ascii_case(text))
    {
        Some(true)
    } else if mapping
        .false_values
        .iter()
        .any(|v| v.trim().eq_ignore_ascii_case(text))
    {
        Some(false)
    } else {
        None
    }
}
fn number(bytes: &[u8], min: f64, max: f64) -> Option<f64> {
    let v: f64 = std::str::from_utf8(bytes).ok()?.trim().parse().ok()?;
    (v.is_finite() && (min..=max).contains(&v)).then_some(v)
}
fn decode_location(bytes: &[u8]) -> Option<Location> {
    let v: Value = serde_json::from_slice(bytes).ok()?;
    let latitude = v.get("latitude").or_else(|| v.get("lat"))?.as_f64()?;
    let longitude = v.get("longitude").or_else(|| v.get("lng"))?.as_f64()?;
    (latitude.is_finite()
        && longitude.is_finite()
        && (-90. ..=90.).contains(&latitude)
        && (-180. ..=180.).contains(&longitude))
    .then_some(Location {
        latitude,
        longitude,
    })
}
