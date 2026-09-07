use crate::{
    app_log::AppLog,
    config::{self, Config, Secrets},
    engine::{Engine, Snapshot, SnapshotIdentity},
    model::Clock,
    mqtt::{self, Session},
};
use rumqttc::{Event, Incoming, Outgoing, SubscribeFilter, SubscribeReasonCode};
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, RwLock,
    },
    time::Duration,
};
use tokio::{sync::watch, time::Instant};

pub struct Runtime {
    pub config: Config,
    pub secrets: Secrets,
    pub identity: SnapshotIdentity,
    pub clock: Clock,
    pub dry: bool,
    pub snapshot: Arc<RwLock<Snapshot>>,
    pub workers_ok: Arc<AtomicBool>,
    pub log: AppLog,
}
impl Runtime {
    pub async fn run(self, mut stop: watch::Receiver<bool>) -> Result<(), String> {
        let epoch = self.clock.monotonic;
        let mut engine = Engine::new(self.config.clone(), self.dry);
        let progress = Arc::new(AtomicU64::new(0));
        let health = self.workers_ok.clone();
        let pulse = progress.clone();
        let stall = self.config.runtime_settings.worker_stall_seconds;
        let mut monitor_stop = stop.clone();
        let monitor = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _=monitor_stop.changed()=>break,
                    _=tokio::time::sleep(Duration::from_millis(250))=>{
                        let age=epoch.elapsed().as_secs_f64()-pulse.load(Ordering::Relaxed) as f64/1000.;
                        if age >= stall { health.store(false,Ordering::Relaxed); }
                    }
                }
            }
        });
        let result = self.control(&mut engine, epoch, &progress, &mut stop).await;
        monitor.abort();
        self.workers_ok.store(false, Ordering::Relaxed);
        engine.stopping = true;
        engine.workers_healthy = false;
        engine.tick(epoch.elapsed().as_secs_f64(), |_| {
            unreachable!("shutdown cannot publish")
        });
        *self.snapshot.write().expect("snapshot lock") =
            engine.snapshot(epoch.elapsed().as_secs_f64(), &self.clock, &self.identity);
        result
    }
    async fn control(
        &self,
        engine: &mut Engine,
        epoch: Instant,
        progress: &AtomicU64,
        stop: &mut watch::Receiver<bool>,
    ) -> Result<(), String> {
        let topics: Vec<_> = config::subscriptions(&self.config).into_keys().collect();
        let mut session: Option<Session> = None;
        let mut retry_at = 0.;
        let mut backoff: f64 = 1.;
        let mut subscription_sent = false;
        let mut subscription_packets: BTreeMap<u16, Vec<String>> = BTreeMap::new();
        let mut sub_started = 0.;
        let mut dispatch_pending: Option<f64> = None;
        let mut accepted_count = 0;
        loop {
            if *stop.borrow() {
                break;
            }
            let now = epoch.elapsed().as_secs_f64();
            progress.store(epoch.elapsed().as_millis() as u64, Ordering::Relaxed);
            if !self.workers_ok.load(Ordering::Relaxed) {
                return Err("Required controller worker stalled".into());
            }
            if session.is_none() && now >= retry_at {
                session = Some(Session::start(
                    &self.config,
                    &self.secrets,
                    &self.identity.client_id,
                    epoch,
                    self.workers_ok.clone(),
                ));
                subscription_sent = false;
                subscription_packets.clear();
                dispatch_pending = None;
                accepted_count = 0;
                self.log.emit(
                    "info",
                    "MQTT_CONNECTING",
                    "Connecting MQTT session",
                    json!({"generation":engine.generation+1}),
                );
            }
            if let Some(s) = &session {
                if s.worker.is_finished() && !s.rx.is_closed() {
                    return Err("MQTT worker exited unexpectedly".into());
                }
                let mqtt_age = now - s.progress.load(Ordering::Relaxed) as f64 / 1000.;
                if mqtt_age >= self.config.runtime_settings.worker_stall_seconds {
                    return Err("MQTT worker stalled".into());
                }
                if engine.connected.value == Some(true) && !subscription_sent {
                    let filters = topics.iter().map(|t| {
                        SubscribeFilter::new(t.clone(), mqtt::qos(self.config.mqtt_settings.qos))
                    });
                    if s.client.try_subscribe_many(filters).is_ok() {
                        subscription_sent = true;
                        sub_started = now;
                    }
                }
                if s.dispatched.load(Ordering::Relaxed) >= accepted_count {
                    dispatch_pending = None;
                }
            }
            let stalled_submission = dispatch_pending
                .is_some_and(|t| now - t >= self.config.mqtt_settings.publish_timeout_seconds);
            let stalled_subscription = subscription_sent
                && !engine.transport_ready()
                && now - sub_started >= self.config.mqtt_settings.publish_timeout_seconds;
            if stalled_submission || stalled_subscription {
                self.log.emit(
                    "error",
                    "MQTT_SESSION_STALLED",
                    "MQTT dispatch or subscription acknowledgement timed out",
                    json!({}),
                );
                if let Some(s) = session.take() {
                    s.stop();
                }
                engine.connection(false, now);
                retry_at = now + backoff;
                backoff = (backoff * 2.).min(30.);
                subscription_sent = false;
                dispatch_pending = None;
            }
            let mut accepted_now = 0;
            engine.tick_with_clock(
                now,
                || epoch.elapsed().as_secs_f64(),
                |p| {
                    if !self.workers_ok.load(Ordering::Relaxed) {
                        return Err(crate::engine::SubmissionError::Rejected);
                    }
                    let result = session
                        .as_ref()
                        .ok_or(crate::engine::SubmissionError::Rejected)
                        .and_then(|s| mqtt::submit(&s.client, p));
                    if result.is_ok() {
                        accepted_now += 1;
                    }
                    result
                },
            );
            if accepted_now > 0 {
                accepted_count += accepted_now;
                dispatch_pending.get_or_insert(now);
            }
            for (key, context) in engine.events.drain(..) {
                let level = if key.ends_with("FAILED") {
                    "error"
                } else if key == "INPUT_REJECTED" {
                    "warn"
                } else if matches!(
                    key,
                    "OUTPUT_BLINK_PHASE"
                        | "INPUT_RECEIVED"
                        | "OUTPUT_DESIRED_CHANGED"
                        | "HEARTBEAT_SUBMITTED"
                        | "STATE_CHANGED"
                        | "CONTROL_STATE_ADMITTED"
                ) {
                    "debug"
                } else {
                    "info"
                };
                self.log.emit(level, key, key.to_ascii_lowercase(), context);
            }
            let mut snapshot =
                engine.snapshot(epoch.elapsed().as_secs_f64(), &self.clock, &self.identity);
            let mqtt_progress = session
                .as_ref()
                .map(|s| s.progress.load(Ordering::Relaxed) as f64 / 1000.);
            snapshot.runtime["workers"]["mqtt"]["last_progress_at"] =
                json!(self.clock.stamp(mqtt_progress));
            snapshot.runtime["workers"]["mqtt"]["expected_wait"] = json!(if session.is_some() {
                "bounded MQTT poll"
            } else {
                "reconnect backoff"
            });
            snapshot.runtime["workers"]["timer_scheduler"]["next_deadline"] =
                json!(self.clock.stamp(Some(engine.next_deadline(now))));
            snapshot.runtime["workers"]["controller"] = json!({"last_progress_at":self.clock.stamp(Some(progress.load(Ordering::Relaxed) as f64 / 1000.)),"healthy":self.workers_ok.load(Ordering::Relaxed),"roles":["evaluator","timer_scheduler","publisher","heartbeat"]});
            *self.snapshot.write().expect("snapshot lock") = snapshot;
            let wake = engine.next_deadline(now).min(if session.is_none() {
                retry_at
            } else {
                now + 0.25
            });
            let event = async {
                match session.as_mut() {
                    Some(s) => s.rx.recv().await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                biased;
                _=stop.changed()=>break,
                received=event=>{
                    match received {
                        Some(Ok(Event::Incoming(Incoming::ConnAck(_))))=>{engine.connection(true,epoch.elapsed().as_secs_f64());backoff=1.;self.log.emit("info","MQTT_CONNECTED","MQTT connected; awaiting subscriptions",json!({"generation":engine.generation}));}
                        Some(Ok(Event::Outgoing(Outgoing::Subscribe(id))))=>{subscription_packets.insert(id,topics.clone());}
                        Some(Ok(Event::Incoming(Incoming::SubAck(ack))))=>{
                            if let Some(requested)=subscription_packets.remove(&ack.pkid) {
                                let granted:Vec<_>=ack.return_codes.iter().map(|c|!matches!(c,SubscribeReasonCode::Failure)).collect();
                                engine.acknowledged(&requested,&granted,epoch.elapsed().as_secs_f64());
                                self.log.emit("info","MQTT_SUBSCRIPTIONS_ACKNOWLEDGED","MQTT subscription acknowledgement received",json!({"all_granted":granted.iter().all(|v|*v)}));
                            }
                        }
                        Some(Ok(Event::Incoming(Incoming::Publish(p))))=>engine.receive(&p.topic,&p.payload,epoch.elapsed().as_secs_f64()),
                        Some(Ok(_))=>(),
                        Some(Err(()))|None=>{
                            if received.is_none() {
                                if let Some(s) = session.as_mut() {
                                    if (&mut s.worker).await.is_err() { return Err("MQTT worker task failed".into()); }
                                    return Err("MQTT worker exited without a transport result".into());
                                }
                            }
                            if let Some(s)=session.take(){s.stop();}
                            let now=epoch.elapsed().as_secs_f64();engine.connection(false,now);retry_at=now+backoff;backoff=(backoff*2.).min(30.);
                            self.log.emit("warn","MQTT_DISCONNECTED","MQTT unavailable; buffered generation discarded",json!({"retry_seconds":backoff}));
                        }
                    }
                }
                _=tokio::time::sleep_until(epoch+Duration::from_secs_f64(wake.max(now+0.001)))=>(),
            }
        }
        if let Some(s) = session {
            s.stop();
        }
        Ok(())
    }
}
