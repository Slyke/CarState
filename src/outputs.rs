use crate::{
    behaviors::{Episode, Filter},
    config::{Behavior, Config, Output, Rule},
    model::{Clock, State, Tracked},
};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub struct OutputRuntime {
    pub rules: Vec<Rule>,
    pub episodes: Vec<Episode>,
    pub selected: Option<usize>,
    pub desired: Tracked<bool>,
    pub last_submitted: Option<bool>,
    pub last_submitted_at: Option<f64>,
    pub submissions: u64,
    pub simulated_value: Option<bool>,
    pub simulated_at: Option<f64>,
    pub simulated_submissions: u64,
    pub eligible_at: f64,
    pub force: bool,
    pub retry_at: Option<f64>,
    pub retry_count: u32,
    pub unresolved: bool,
    pub last_error: Option<(&'static str, f64)>,
    pub pending_reason: Option<&'static str>,
}
impl OutputRuntime {
    pub fn new(output: &Output, hold: f64) -> Self {
        let mut rules = output.rules();
        rules.sort_by_key(|r| std::cmp::Reverse(r.priority));
        let episodes = rules.iter().map(|_| Episode::default()).collect();
        Self {
            rules,
            episodes,
            selected: None,
            desired: Tracked::default(),
            last_submitted: None,
            last_submitted_at: None,
            submissions: 0,
            simulated_value: None,
            simulated_at: None,
            simulated_submissions: 0,
            eligible_at: hold,
            force: true,
            retry_at: None,
            retry_count: 0,
            unresolved: false,
            last_error: None,
            pending_reason: Some("startup"),
        }
    }
    pub fn resync(&mut self, now: f64, hold: f64, settle: bool, reason: &'static str) {
        self.force = true;
        self.pending_reason = Some(reason);
        if settle {
            self.eligible_at = self.eligible_at.max(now + hold);
        }
    }
    pub fn select(
        &mut self,
        output: &Output,
        states: &BTreeMap<State, Filter>,
        now: f64,
        can_attempt: bool,
        dry: bool,
    ) -> Vec<(&'static str, Value)> {
        let mut events = Vec::new();
        for (i, r) in self.rules.iter().enumerate() {
            let trigger = states[&r.state].admitted.value.map(|v| v == r.when);
            let episode = &mut self.episodes[i];
            let was_started = episode.start.is_some();
            let was_exhausted = episode.exhausted;
            episode.update(trigger, now);
            if was_started && episode.start.is_none() {
                events.push(("OUTPUT_BEHAVIOR_CANCELLED", json!({"rule":r.name})));
            }
            if was_exhausted && !episode.exhausted {
                events.push(("OUTPUT_BEHAVIOR_REARMED", json!({"rule":r.name})));
            }
            if !was_exhausted && episode.exhausted {
                events.push(("OUTPUT_BEHAVIOR_EXPIRED", json!({"rule":r.name})));
            }
        }
        let selected = self
            .rules
            .iter()
            .enumerate()
            .find(|(i, r)| {
                states[&r.state].admitted.value == Some(r.when) && !self.episodes[*i].exhausted
            })
            .map(|(i, _)| i);
        if self.selected != selected {
            events.push(("OUTPUT_RULE_SELECTED",json!({"previous_rule":self.selected.map(|i|&self.rules[i].name),"rule":selected.map(|i|&self.rules[i].name),"preempted":self.selected.is_some() && selected.is_some()})));
        }
        self.selected = selected;
        let value = if let Some(i) = selected {
            match self.rules[i].behavior {
                Behavior::Steady { value } => Some(value),
                Behavior::BlinkFor {
                    interval_seconds,
                    duration_seconds,
                } => {
                    if self.episodes[i].start.is_none()
                        && can_attempt
                        && now >= self.eligible_at
                        && self.retry_at.is_none_or(|t| now >= t)
                    {
                        self.episodes[i].start(now, duration_seconds);
                        events.push((
                            "OUTPUT_BEHAVIOR_STARTED",
                            json!({"rule":self.rules[i].name,"duration_seconds":duration_seconds}),
                        ));
                    }
                    Some(self.episodes[i].phase(now, interval_seconds))
                }
            }
        } else if self
            .rules
            .iter()
            .any(|r| states[&r.state].admitted.value.is_some())
        {
            Some(output.default_value)
        } else {
            None
        };
        let changed = self.desired.set(value, now);
        if changed {
            events.push(("OUTPUT_DESIRED_CHANGED", json!({"value":value})));
        }
        let previous = if dry {
            self.simulated_value
        } else {
            self.last_submitted
        };
        if value.is_none() || (value == previous && !self.force) {
            self.unresolved = false;
            self.retry_at = None;
            self.retry_count = 0;
        }
        events
    }
    pub fn pending(&self, dry: bool) -> bool {
        self.desired.value.is_some()
            && (self.force
                || self.desired.value
                    != if dry {
                        self.simulated_value
                    } else {
                        self.last_submitted
                    })
    }
    pub fn due(&self, now: f64, dry: bool) -> Option<bool> {
        (self.pending(dry) && now >= self.eligible_at && self.retry_at.is_none_or(|t| now >= t))
            .then_some(self.desired.value)
            .flatten()
    }
    pub fn accepted(&mut self, value: bool, now: f64, hold: f64, dry: bool) {
        if dry {
            self.simulated_value = Some(value);
            self.simulated_at = Some(now);
            self.simulated_submissions += 1;
        } else {
            self.last_submitted = Some(value);
            self.last_submitted_at = Some(now);
            self.submissions += 1;
        }
        self.eligible_at = now + hold;
        self.force = false;
        self.unresolved = false;
        self.retry_count = 0;
        self.retry_at = None;
        self.pending_reason = None;
    }
    pub fn failed(&mut self, now: f64, c: &Config, ambiguous: bool) {
        self.retry_count = self.retry_count.saturating_add(1);
        self.unresolved = true;
        let backoff = (c.mqtt_settings.publish_retry_initial_seconds
            * 2_f64.powi(self.retry_count.min(32) as i32 - 1))
        .min(c.mqtt_settings.publish_retry_max_seconds);
        self.retry_at = Some(now + backoff);
        self.last_error = Some((
            if ambiguous {
                "publish_timeout"
            } else {
                "publish_rejected"
            },
            now,
        ));
        self.pending_reason = Some("retry");
        if ambiguous {
            self.eligible_at = self
                .eligible_at
                .max(now + c.output_settings.min_hold_seconds);
        }
    }
    pub fn snapshot(
        &self,
        o: &Output,
        c: &Config,
        controls: &BTreeMap<State, Filter>,
        now: f64,
        clock: &Clock,
        dry: bool,
    ) -> Value {
        let rules: Vec<_> = self.rules.iter().enumerate().map(|(i,r)| {
            let e = &self.episodes[i]; let matching = controls[&r.state].admitted.value.map(|v|v==r.when);
            let next = if self.selected == Some(i) { match r.behavior { Behavior::BlinkFor {interval_seconds,..} => e.next_phase(now,interval_seconds), _ => None } } else { None };
            json!({"name":r.name,"state":r.state,"trigger_value":controls[&r.state].admitted.value,"priority":r.priority,"behavior":r.behavior,"selected":self.selected==Some(i),"status":if e.exhausted {"exhausted"} else if matching.is_none() {"suspended"} else if matching==Some(false) {"armed"} else if self.selected!=Some(i) {"preempted"} else {"selected"},"episode_start":clock.stamp(e.start),"episode_deadline":clock.stamp(e.deadline),"next_transition":clock.stamp(next),"remaining_seconds":e.deadline.map(|d|(d-now).max(0.)),"exhausted":e.exhausted,"armed":e.start.is_none() && !e.exhausted})
        }).collect();
        json!({"name":o.name,"color":o.color,"topic":o.topic,"qos":c.mqtt_settings.qos,"retain":c.mqtt_settings.retain_commands,"selected_rule":self.selected.map(|i|&self.rules[i].name),"active_macro":self.selected.map(|i|match self.rules[i].behavior {Behavior::Steady{..}=>"steady",Behavior::BlinkFor{..}=>"blink_for"}),"desired":self.desired.snapshot(clock),"desired_payload":self.desired.value.map(|v|o.payload(v)),"pending_submission":self.pending(dry),"pending_command":self.pending(dry).then_some(self.desired.value),"pending_reason":self.pending_reason,"last_submitted_payload":self.last_submitted.map(|v|o.payload(v)),"last_submitted_at":clock.stamp(self.last_submitted_at),"submission_count":self.submissions,"next_eligible_submission":clock.stamp(Some(self.eligible_at)),"hold_remaining_seconds":(self.eligible_at-now).max(0.),"retry_count":self.retry_count,"retry_deadline":clock.stamp(self.retry_at),"submission_failed":self.unresolved,"last_error":self.last_error.map(|(code,t)|json!({"code":code,"at":clock.stamp(Some(t))})),"rules":rules,"would_publish":if dry {Some(json!({"payload":self.simulated_value.map(|v|o.payload(v)),"at":clock.stamp(self.simulated_at),"count":self.simulated_submissions}))} else {None}})
    }
}
