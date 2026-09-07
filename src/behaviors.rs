use crate::{
    config::JitterSettings,
    model::{Clock, Tracked, Truth},
};
use serde_json::{json, Value};

#[derive(Default)]
pub struct Filter {
    pub admitted: Tracked<bool>,
    pub last_known: Truth,
    pub last_known_at: Option<f64>,
    pub burst: u32,
    pub cooldown: Option<f64>,
    pub pending: Truth,
}
impl Filter {
    pub fn update(&mut self, raw: Truth, now: f64, settings: &JitterSettings) {
        self.pending = raw;
        let Some(value) = raw else {
            self.admitted.set(None, now);
            return;
        };
        if self.cooldown.is_some_and(|d| now >= d) {
            self.cooldown = None;
            self.burst = 0;
        }
        if self.cooldown.is_none()
            && self
                .last_known_at
                .is_some_and(|t| now - t >= settings.cooldown_seconds)
        {
            self.burst = 0;
        }
        if self.last_known == Some(value) {
            self.admitted.set(Some(value), now);
            self.pending = None;
            return;
        }
        if self.cooldown.is_some() {
            return;
        }
        if self.last_known.is_some() {
            self.burst += 1;
        }
        self.last_known = Some(value);
        self.last_known_at = Some(now);
        self.admitted.set(Some(value), now);
        self.pending = None;
        if self.burst >= settings.max_changes {
            self.cooldown = Some(now + settings.cooldown_seconds);
        }
    }
    pub fn snapshot(&self, c: &Clock, s: &JitterSettings) -> Value {
        json!({"value":self.admitted.value,"last_changed_at":c.stamp(self.admitted.last_changed),"last_admitted_known_value":self.last_known,"last_admitted_known_at":c.stamp(self.last_known_at),"burst_count":self.burst,"burst_limit":s.max_changes,"cooldown_deadline":c.stamp(self.cooldown),"latest_pending_value":self.pending})
    }
}
#[derive(Default)]
pub struct Episode {
    pub start: Option<f64>,
    pub deadline: Option<f64>,
    pub exhausted: bool,
}
impl Episode {
    pub fn update(&mut self, matches: Truth, now: f64) {
        if matches == Some(false) {
            self.start = None;
            self.deadline = None;
            self.exhausted = false;
        } else if self.deadline.is_some_and(|d| now >= d) {
            self.exhausted = true;
        }
    }
    pub fn start(&mut self, now: f64, duration: f64) {
        if self.start.is_none() {
            self.start = Some(now);
            self.deadline = Some(now + duration);
        }
    }
    pub fn phase(&self, now: f64, interval: f64) -> bool {
        self.start
            .is_none_or(|start| ((now - start) / (interval / 2.)).floor() % 2. == 0.)
    }
    pub fn next_phase(&self, now: f64, interval: f64) -> Option<f64> {
        let start = self.start?;
        let half = interval / 2.;
        let next = start + (((now - start) / half).floor() + 1.) * half;
        (!self.exhausted).then_some(next.min(self.deadline?))
    }
}
