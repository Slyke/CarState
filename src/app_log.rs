use rand::RngCore;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};
use styleguide_logger::{ErrorCodeMap, ErrorOptions, LogOptions, Logger, LoggingConfig};
use tokio::sync::mpsc;

pub const K8S_KEYS: [&str; 6] = [
    "K8S_POD_NAME",
    "K8S_DEPLOYMENT",
    "K8S_NAMESPACE",
    "K8S_POD_IP",
    "K8S_POD_IPS",
    "K8S_NODE_NAME",
];
pub fn kubernetes(env: &BTreeMap<String, String>) -> Value {
    Value::Object(
        K8S_KEYS
            .into_iter()
            .filter_map(|k| {
                env.get(k)
                    .filter(|v| !v.is_empty())
                    .map(|v| (k.into(), json!(v)))
            })
            .collect(),
    )
}
pub fn client_id(
    configured: &str,
    dry: bool,
    mut fill: impl FnMut(&mut [u8]) -> Result<(), String>,
) -> Result<(String, bool), String> {
    if !dry && !configured.trim().is_empty() {
        return Ok((configured.into(), false));
    }
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut suffix = String::new();
    for _ in 0..1024 {
        let mut bytes = [0; 16];
        fill(&mut bytes)?;
        for b in bytes {
            if b < 252 {
                suffix.push(alphabet[(b % 36) as usize] as char);
                if suffix.len() == 8 {
                    return Ok((
                        format!(
                            "{}{}",
                            if dry { "carstate_dryrun_" } else { "carstate_" },
                            suffix
                        ),
                        true,
                    ));
                }
            }
        }
    }
    Err("Operating-system random source failed to generate identity".into())
}
pub fn generate_id(configured: &str, dry: bool) -> Result<(String, bool), String> {
    client_id(configured, dry, |b| {
        rand::rngs::OsRng
            .try_fill_bytes(b)
            .map_err(|_| "Operating-system random source unavailable".into())
    })
}
pub fn bootstrap() -> Logger {
    let config = LoggingConfig::from_json5_with_environment(
        r#"{sinks:{console:{enabled:true,format:'json',levels:['info','warn','error','debug']}}}"#,
        &|_| None,
    )
    .expect("static bootstrap configuration");
    Logger::new(config, ErrorCodeMap::new()).expect("console logger")
}
#[derive(Clone)]
pub struct Identity {
    pub instance_id: String,
    pub hostname: Option<String>,
    pub pid: u32,
}
impl Identity {
    pub fn new(id: &str, env: &BTreeMap<String, String>) -> Self {
        let nonempty = |key| env.get(key).filter(|s| !s.is_empty()).cloned();
        Self {
            instance_id: nonempty("INSTANCE_ID")
                .or_else(|| nonempty("K8S_POD_NAME"))
                .or_else(|| nonempty("HOSTNAME"))
                .unwrap_or_else(|| id.into()),
            hostname: nonempty("HOSTNAME"),
            pid: std::process::id(),
        }
    }
    pub fn enrich(&self, mut context: Value) -> Value {
        if !context.is_object() {
            context = json!({});
        }
        context["instanceId"] = json!(self.instance_id);
        context["pid"] = json!(self.pid);
        if let Some(host) = &self.hostname {
            context["hostname"] = json!(host);
        }
        context
    }
}
pub struct Entry {
    pub level: &'static str,
    pub key: &'static str,
    pub message: String,
    pub context: Value,
    pub correlation: Option<String>,
}
#[derive(Clone)]
pub struct AppLog {
    tx: mpsc::Sender<Entry>,
    dropped: Arc<AtomicU64>,
}
impl AppLog {
    pub fn start(logger: Logger, identity: Identity) -> (Self, tokio::task::JoinHandle<()>) {
        let (tx, mut rx) = mpsc::channel::<Entry>(1024);
        let dropped = Arc::new(AtomicU64::new(0));
        let counts = dropped.clone();
        let worker = tokio::spawn(async move {
            let console = bootstrap();
            while let Some(e) = rx.recv().await {
                let context = identity.enrich(e.context);
                let failures = if e.level == "error" {
                    logger
                        .generate_error(ErrorOptions {
                            caller: "carstate".into(),
                            reason: e.message,
                            error_key: e.key.into(),
                            context: Some(context),
                            correlation_id: e.correlation,
                            ..Default::default()
                        })
                        .await
                        .failures
                } else {
                    logger
                        .generate_log(LogOptions {
                            level: e.level.into(),
                            caller: "carstate".into(),
                            logger_key: Some(e.key.into()),
                            message: e.message,
                            context: Some(context),
                            correlation_id: e.correlation,
                            ..Default::default()
                        })
                        .await
                        .failures
                };
                let dropped = counts.swap(0, Ordering::Relaxed);
                if !failures.is_empty() || dropped > 0 {
                    console.generate_log(LogOptions {level:"warn".into(),caller:"carstate::logging".into(),logger_key:Some("LOG_DELIVERY_DEGRADED".into()),message:"Log delivery degraded".into(),context:Some(identity.enrich(json!({"failed_sinks":failures.iter().map(|f|f.sink).collect::<Vec<_>>(),"dropped_events":dropped}))),..Default::default()}).await;
                }
            }
        });
        (Self { tx, dropped }, worker)
    }
    pub fn emit(
        &self,
        level: &'static str,
        key: &'static str,
        message: impl Into<String>,
        context: Value,
    ) {
        self.correlated(level, key, message, context, None);
    }
    pub fn correlated(
        &self,
        level: &'static str,
        key: &'static str,
        message: impl Into<String>,
        context: Value,
        correlation: Option<String>,
    ) {
        if self
            .tx
            .try_send(Entry {
                level,
                key,
                message: message.into(),
                context,
                correlation,
            })
            .is_err()
        {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}
