//! Configuration errors deliberately contain field names, never input values.
use crate::model::State;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};
use styleguide_logger::{
    config::parse_json5_with_environment, error_codes, ErrorCodeMap, LoggingConfig,
};

pub type ConfigResult<T> = Result<T, String>;
#[derive(Clone, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub http: HttpSettings,
    pub mqtt_settings: MqttSettings,
    pub inputs: Inputs,
    pub state_settings: Vec<StateSettings>,
    pub outputs: Vec<Output>,
    pub output_devices: Vec<Device>,
    pub jitter: JitterSettings,
    pub output_settings: OutputSettings,
    pub runtime_settings: RuntimeSettings,
    pub telemetry_settings: TelemetrySettings,
    pub history_settings: HistorySettings,
    pub heartbeat: HeartbeatSettings,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct HttpSettings {
    pub use_http: bool,
    pub interface: String,
    #[serde(deserialize_with = "port")]
    pub port: u16,
    pub state_endpoint_enabled: bool,
}
impl Default for HttpSettings {
    fn default() -> Self {
        Self {
            use_http: true,
            interface: "0.0.0.0".into(),
            port: 3000,
            state_endpoint_enabled: false,
        }
    }
}
fn port<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u16, D::Error> {
    match Value::deserialize(d)? {
        Value::String(s) => s
            .parse()
            .map_err(|_| serde::de::Error::custom("invalid port")),
        v => serde_json::from_value(v).map_err(|_| serde::de::Error::custom("invalid port")),
    }
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct MqttSettings {
    pub ip: String,
    pub port: u16,
    pub use_tls: bool,
    pub validate_certs: bool,
    pub keep_alive_seconds: u64,
    pub qos: u8,
    pub retain_commands: bool,
    pub publish_retry_initial_seconds: f64,
    pub publish_retry_max_seconds: f64,
    pub publish_timeout_seconds: f64,
}
impl Default for MqttSettings {
    fn default() -> Self {
        Self {
            ip: "localhost".into(),
            port: 1883,
            use_tls: false,
            validate_certs: true,
            keep_alive_seconds: 30,
            qos: 1,
            retain_commands: false,
            publish_retry_initial_seconds: 1.,
            publish_retry_max_seconds: 30.,
            publish_timeout_seconds: 5.,
        }
    }
}
#[derive(Clone, Deserialize, Default)]
#[serde(default)]
pub struct Secrets {
    pub mqtt_username: String,
    pub mqtt_password: String,
    pub mqtt_client_id: String,
    #[serde(deserialize_with = "optional_string")]
    pub http_state_token: String,
}
fn optional_string<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(d)?.unwrap_or_default())
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct JitterSettings {
    pub max_changes: u32,
    pub cooldown_seconds: f64,
}
impl Default for JitterSettings {
    fn default() -> Self {
        Self {
            max_changes: 3,
            cooldown_seconds: 10.,
        }
    }
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct OutputSettings {
    pub min_hold_seconds: f64,
}
impl Default for OutputSettings {
    fn default() -> Self {
        Self {
            min_hold_seconds: 1.,
        }
    }
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct RuntimeSettings {
    pub worker_stall_seconds: f64,
}
impl Default for RuntimeSettings {
    fn default() -> Self {
        Self {
            worker_stall_seconds: 30.,
        }
    }
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct TelemetrySettings {
    pub timeout_seconds: Option<f64>,
}
impl Default for TelemetrySettings {
    fn default() -> Self {
        Self {
            timeout_seconds: Some(600.),
        }
    }
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct HistorySettings {
    pub max_entries: usize,
}
impl Default for HistorySettings {
    fn default() -> Self {
        Self { max_entries: 50 }
    }
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct HeartbeatSettings {
    pub enabled: bool,
    pub topic: Option<String>,
    pub payload: Option<String>,
    pub interval_seconds: f64,
    pub timeout_seconds: f64,
}
impl Default for HeartbeatSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            topic: None,
            payload: None,
            interval_seconds: 120.,
            timeout_seconds: 600.,
        }
    }
}
#[derive(Clone, Deserialize, Default)]
#[serde(default)]
pub struct Inputs {
    pub location: Option<LocationInput>,
    pub battery: Option<NumericInput>,
    pub charging: Option<BooleanInput>,
    pub plugged_in: Option<BooleanInput>,
    pub charge_complete: Option<BooleanInput>,
    pub parked: Option<BooleanInput>,
    pub locked: Option<BooleanInput>,
    pub online: Option<BooleanInput>,
    pub source_healthy: Option<BooleanInput>,
    pub faults: Vec<FaultInput>,
}
impl Inputs {
    pub fn booleans(&self) -> Vec<(&str, &BooleanInput)> {
        [
            ("charging", &self.charging),
            ("plugged_in", &self.plugged_in),
            ("charge_complete", &self.charge_complete),
            ("parked", &self.parked),
            ("locked", &self.locked),
            ("online", &self.online),
            ("source_healthy", &self.source_healthy),
        ]
        .into_iter()
        .filter_map(|(name, value)| value.as_ref().map(|v| (name, v)))
        .collect()
    }
}
#[derive(Clone, Deserialize, Serialize)]
pub struct BooleanInput {
    pub topic: String,
    pub true_values: Vec<String>,
    pub false_values: Vec<String>,
    #[serde(default)]
    pub stale_after_seconds: Option<f64>,
}
#[derive(Clone, Deserialize)]
pub struct FaultInput {
    pub name: String,
    #[serde(flatten)]
    pub mapping: BooleanInput,
}
#[derive(Clone, Deserialize)]
pub struct NumericInput {
    pub topic: String,
    #[serde(default)]
    pub stale_after_seconds: Option<f64>,
}
#[derive(Clone, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum LocationInput {
    Json {
        topic: String,
        #[serde(default)]
        stale_after_seconds: Option<f64>,
    },
    Split {
        latitude_topic: String,
        longitude_topic: String,
        #[serde(default = "default_skew")]
        max_coordinate_skew_seconds: f64,
        #[serde(default)]
        stale_after_seconds: Option<f64>,
    },
}
fn default_skew() -> f64 {
    10.
}
impl LocationInput {
    pub fn stale(&self) -> Option<f64> {
        match self {
            Self::Json {
                stale_after_seconds,
                ..
            }
            | Self::Split {
                stale_after_seconds,
                ..
            } => *stale_after_seconds,
        }
    }
}
#[derive(Clone, Deserialize, Default)]
#[serde(default)]
pub struct StateSettings {
    pub name: String,
    pub target_latitude: Option<f64>,
    pub target_longitude: Option<f64>,
    pub outer_radius_meters: Option<f64>,
    pub inner_radius_meters: Option<f64>,
    pub battery_low_percent: Option<f64>,
    pub battery_low_is_fault: bool,
}
#[derive(Clone, Deserialize)]
pub struct Device {
    pub name: String,
    pub availability: BooleanInput,
}
#[derive(Clone, Deserialize)]
pub struct Output {
    pub name: String,
    pub color: Option<String>,
    pub topic: String,
    pub device: Option<String>,
    pub true_payload: String,
    pub false_payload: String,
    #[serde(default)]
    pub default_value: bool,
    pub state: Option<State>,
    pub rules: Option<Vec<Rule>>,
}
impl Output {
    pub fn rules(&self) -> Vec<Rule> {
        self.rules.clone().unwrap_or_else(|| {
            self.state
                .map(|state| {
                    vec![Rule {
                        name: "state".into(),
                        state,
                        when: true,
                        priority: 0,
                        start_on_match: false,
                        restart_on: Vec::new(),
                        behavior: Behavior::Steady { value: true },
                    }]
                })
                .unwrap_or_default()
        })
    }
    pub fn payload(&self, value: bool) -> &str {
        if value {
            &self.true_payload
        } else {
            &self.false_payload
        }
    }
}
#[derive(Clone, Deserialize, Serialize)]
pub struct Rule {
    pub name: String,
    pub state: State,
    #[serde(default = "yes")]
    pub when: bool,
    pub priority: i64,
    /// Anchor a timed behavior to its admitted trigger, even while preempted/offline.
    #[serde(default)]
    pub start_on_match: bool,
    #[serde(default)]
    pub restart_on: Vec<StateTransition>,
    pub behavior: Behavior,
}
#[derive(Clone, Deserialize, Serialize)]
pub struct StateTransition {
    pub state: State,
    pub from: bool,
    pub to: bool,
}
fn yes() -> bool {
    true
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "macro", rename_all = "snake_case")]
pub enum Behavior {
    Steady {
        value: bool,
    },
    BlinkFor {
        interval_seconds: f64,
        duration_seconds: f64,
    },
    SteadyFor {
        value: bool,
        duration_seconds: f64,
    },
}
impl Behavior {
    pub fn duration(&self) -> Option<f64> {
        match self {
            Self::Steady { .. } => None,
            Self::BlinkFor {
                duration_seconds, ..
            }
            | Self::SteadyFor {
                duration_seconds, ..
            } => Some(*duration_seconds),
        }
    }
    pub fn name(&self) -> &'static str {
        match self {
            Self::Steady { .. } => "steady",
            Self::BlinkFor { .. } => "blink_for",
            Self::SteadyFor { .. } => "steady_for",
        }
    }
}

pub struct Loaded {
    pub config: Config,
    pub secrets: Secrets,
    pub logging: LoggingConfig,
    pub codes: ErrorCodeMap,
    pub warnings: Vec<String>,
    pub config_path: String,
    pub secrets_path: String,
}
pub fn load() -> ConfigResult<Loaded> {
    let config_path =
        std::env::var("CARSTATE_CONFIG_PATH").unwrap_or_else(|_| "./config/carstate.json5".into());
    let secrets_path =
        std::env::var("CARSTATE_SECRETS_PATH").unwrap_or_else(|_| "./config/secrets.json5".into());
    let text =
        std::fs::read_to_string(&config_path).map_err(|_| "Cannot read configuration file")?;
    let secret_text =
        std::fs::read_to_string(&secrets_path).map_err(|_| "Cannot read secrets file")?;
    let env: BTreeMap<_, _> = std::env::vars().collect();
    let (config, secrets, mut warnings) = parse(&text, &secret_text, &env)?;
    let mut raw: Value = json5::from_str(&text).map_err(|_| "Invalid configuration JSON5")?;
    if raw.get("logging").is_some_and(|v| !v.is_object())
        || raw
            .pointer("/logging/kubernetes")
            .is_some_and(|v| !v.is_object())
    {
        return Err("logging and logging.kubernetes must be objects".into());
    }
    let explicit = raw.pointer("/logging/kubernetes/enabled").is_some()
        || env.contains_key("LOG_K8S_METADATA_ENABLED");
    if !explicit
        && crate::app_log::kubernetes(&env)
            .as_object()
            .is_some_and(|v| !v.is_empty())
    {
        if raw.get("logging").is_none() {
            raw["logging"] = json!({});
        }
        if raw["logging"].get("kubernetes").is_none() {
            raw["logging"]["kubernetes"] = json!({});
        }
        raw["logging"]["kubernetes"]["enabled"] = true.into();
    }
    let logging =
        LoggingConfig::from_json5_with_environment(&raw.to_string(), &|key| env.get(key).cloned())
            .map_err(|_| "Invalid logging configuration")?;
    styleguide_logger::validate_transport_settings(&logging)
        .map_err(|_| "Invalid logging transport settings")?;
    let catalog = logging
        .error_file
        .as_deref()
        .unwrap_or("./config/errors.json5");
    let codes = error_codes::load_error_codes(catalog)
        .map_err(|_| "Invalid or unreadable error catalog")?;
    if secrets.mqtt_client_id.trim().is_empty() {
        warnings.push(
            "Blank mqtt_client_id: normal startup generates carstate_XXXXXXXX once per process"
                .into(),
        );
    }
    Ok(Loaded {
        config,
        secrets,
        logging,
        codes,
        warnings,
        config_path,
        secrets_path,
    })
}
pub fn parse(
    text: &str,
    secret_text: &str,
    env: &BTreeMap<String, String>,
) -> ConfigResult<(Config, Secrets, Vec<String>)> {
    for document in [text, secret_text] {
        let parsed = json5::from_str::<crate::config_value::FiniteValue>(document)
            .map_err(|_| "Invalid JSON5 or non-finite configuration number")?;
        if !parsed.0.is_object() {
            return Err("Configuration and secrets must be objects".into());
        }
    }
    let mut raw: Value = parse_json5_with_environment(text, &|key| env.get(key).cloned())
        .map_err(|_| "Invalid configuration JSON5")?;
    let secret_raw: Value = parse_json5_with_environment(secret_text, &|key| env.get(key).cloned())
        .map_err(|_| "Invalid secrets JSON5")?;
    let mut warnings = unknown_keys(&raw, "");
    warnings.extend(unknown_keys(&secret_raw, "secrets"));
    for (variable, field) in [
        ("CARSTATE_HTTP_PORT", "port"),
        ("CARSTATE_HTTP_INT", "interface"),
        ("CARSTATE_USE_HTTP", "use_http"),
    ] {
        if let Some(value) = env.get(variable) {
            if raw.get("http").is_some_and(|v| !v.is_object()) {
                return Err("http must be an object".into());
            }
            if raw.get("http").is_none() {
                raw["http"] = json!({});
            }
            raw["http"][field] = if field == "use_http" {
                json!(value
                    .parse::<bool>()
                    .map_err(|_| "Invalid CARSTATE_USE_HTTP")?)
            } else {
                json!(value)
            };
        }
    }
    let config: Config = serde_json::from_value(raw)
        .map_err(|_| "Configuration has a missing field, unknown enum, or incorrect field type")?;
    let secrets: Secrets =
        serde_json::from_value(secret_raw).map_err(|_| "Secrets have an incorrect field type")?;
    validate(&config, &secrets)?;
    for output in &config.outputs {
        if output
            .rules()
            .iter()
            .all(|r| !crate::rules::possible(r.state, &config))
        {
            warnings.push(format!(
                "Output {} has no producible trigger with configured inputs/settings",
                output.name
            ));
        }
    }
    Ok((config, secrets, warnings))
}
pub fn seconds(value: f64, minimum: f64, field: &str) -> ConfigResult<()> {
    if !value.is_finite() || value <= 0. || value < minimum {
        return Err(format!(
            "{field} must be finite, positive and at least {minimum}"
        ));
    }
    let duration =
        Duration::try_from_secs_f64(value).map_err(|_| format!("{field} is too large"))?;
    Instant::now()
        .checked_add(duration)
        .ok_or_else(|| format!("{field} deadline is too large"))?;
    // UTC projections use chrono's signed milliseconds as well as monotonic deadlines.
    if value > 1e12 {
        return Err(format!("{field} deadline is too large"));
    }
    Ok(())
}
fn topic(value: &str) -> ConfigResult<()> {
    if value.trim().is_empty()
        || value.len() > u16::MAX as usize
        || value.contains(['+', '#', '\0'])
    {
        return Err("MQTT topics must be non-empty exact topics without wildcards or NUL".into());
    }
    Ok(())
}
fn unique<'a>(values: impl Iterator<Item = &'a str>, kind: &str) -> ConfigResult<()> {
    let mut seen = BTreeSet::new();
    for v in values {
        if v.trim().is_empty() || !seen.insert(v) {
            return Err(format!("{kind} must have unique non-empty names"));
        }
    }
    Ok(())
}
fn boolean(input: &BooleanInput) -> ConfigResult<()> {
    topic(&input.topic)?;
    let normalize = |v: &String| v.trim().to_ascii_lowercase();
    let a: BTreeSet<_> = input.true_values.iter().map(normalize).collect();
    let b: BTreeSet<_> = input.false_values.iter().map(normalize).collect();
    if a.is_empty() || b.is_empty() || a.contains("") || b.contains("") || !a.is_disjoint(&b) {
        return Err("Boolean mappings must be non-empty and disjoint after normalization".into());
    }
    if let Some(s) = input.stale_after_seconds {
        seconds(s, 0., "stale_after_seconds")?;
    }
    Ok(())
}
pub fn validate(c: &Config, s: &Secrets) -> ConfigResult<()> {
    if c.http.port == 0 || c.mqtt_settings.port == 0 {
        return Err("Ports must be between 1 and 65535".into());
    }
    if c.http.interface.trim().is_empty() || c.mqtt_settings.ip.trim().is_empty() {
        return Err("HTTP interface and broker host must be non-empty".into());
    }
    if c.http.state_endpoint_enabled && !c.http.use_http {
        return Err("http.state_endpoint_enabled requires http.use_http = true".into());
    }
    if c.mqtt_settings.qos > 1
        || c.mqtt_settings.keep_alive_seconds < 1
        || c.mqtt_settings.keep_alive_seconds > 65535
    {
        return Err("Invalid MQTT QoS or keep alive".into());
    }
    if s.mqtt_client_id.len() > 65535 || s.mqtt_client_id.contains('\0') {
        return Err("Invalid MQTT client ID".into());
    }
    if s.mqtt_username.len() > 65535 || s.mqtt_password.len() > 65535 {
        return Err("MQTT credentials exceed protocol limits".into());
    }
    if c.jitter.max_changes == 0 {
        return Err("jitter.max_changes must be positive".into());
    }
    if let Some(timeout) = c.telemetry_settings.timeout_seconds {
        seconds(timeout, 1., "telemetry_settings.timeout_seconds")?;
    }
    for (v, min, name) in [
        (c.jitter.cooldown_seconds, 1., "jitter.cooldown_seconds"),
        (
            c.output_settings.min_hold_seconds,
            1.,
            "output_settings.min_hold_seconds",
        ),
        (
            c.runtime_settings.worker_stall_seconds,
            1.,
            "runtime_settings.worker_stall_seconds",
        ),
        (
            c.mqtt_settings.publish_retry_initial_seconds,
            0.,
            "publish_retry_initial_seconds",
        ),
        (
            c.mqtt_settings.publish_retry_max_seconds,
            0.,
            "publish_retry_max_seconds",
        ),
        (
            c.mqtt_settings.publish_timeout_seconds,
            0.,
            "publish_timeout_seconds",
        ),
    ] {
        seconds(v, min, name)?;
    }
    if c.mqtt_settings.publish_retry_max_seconds < c.mqtt_settings.publish_retry_initial_seconds {
        return Err("Publish retry maximum must be at least initial delay".into());
    }
    for (_, input) in c.inputs.booleans() {
        boolean(input)?;
    }
    for fault in &c.inputs.faults {
        boolean(&fault.mapping)?;
        if !fault
            .name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Err("Fault names must be identifiers".into());
        }
    }
    unique(c.inputs.faults.iter().map(|f| f.name.as_str()), "Faults")?;
    if let Some(b) = &c.inputs.battery {
        topic(&b.topic)?;
        if let Some(s) = b.stale_after_seconds {
            seconds(s, 0., "battery.stale_after_seconds")?;
        }
    }
    if let Some(location) = &c.inputs.location {
        if let Some(s) = location.stale() {
            seconds(s, 0., "location.stale_after_seconds")?;
        }
        match location {
            LocationInput::Json { topic: t, .. } => topic(t)?,
            LocationInput::Split {
                latitude_topic,
                longitude_topic,
                max_coordinate_skew_seconds,
                ..
            } => {
                topic(latitude_topic)?;
                topic(longitude_topic)?;
                if latitude_topic == longitude_topic {
                    return Err("Split GPS topics must differ".into());
                }
                seconds(
                    *max_coordinate_skew_seconds,
                    0.,
                    "max_coordinate_skew_seconds",
                )?;
            }
        }
    }
    unique(
        c.state_settings.iter().map(|e| e.name.as_str()),
        "State settings",
    )?;
    for e in &c.state_settings {
        match (
            e.target_latitude,
            e.target_longitude,
            e.inner_radius_meters,
            e.outer_radius_meters,
        ) {
            (None, None, None, None) => (),
            (Some(lat), Some(lon), Some(inner), Some(outer))
                if lat.is_finite()
                    && lon.is_finite()
                    && (-90. ..=90.).contains(&lat)
                    && (-180. ..=180.).contains(&lon)
                    && inner.is_finite()
                    && outer.is_finite()
                    && inner > 0.
                    && inner < outer => {}
            _ => {
                return Err(
                    "Each supplied geofence needs valid coordinates and 0 < inner < outer radii"
                        .into(),
                )
            }
        }
        if e.battery_low_percent
            .is_some_and(|v| !v.is_finite() || !(0. ..=100.).contains(&v))
        {
            return Err("Invalid battery_low_percent".into());
        }
    }
    unique(
        c.output_devices.iter().map(|e| e.name.as_str()),
        "Output devices",
    )?;
    for d in &c.output_devices {
        boolean(&d.availability)?;
    }
    unique(c.outputs.iter().map(|o| o.name.as_str()), "Outputs")?;
    unique(c.outputs.iter().map(|o| o.topic.as_str()), "Output topics")?;
    let subscriptions = subscriptions(c);
    if subscriptions
        .values()
        .flatten()
        .all(|r| r.starts_with("device:"))
        || c.outputs.is_empty()
    {
        return Err("At least one vehicle input and one output are required".into());
    }
    let mut publish_topics = BTreeSet::new();
    for o in &c.outputs {
        topic(&o.topic)?;
        publish_topics.insert(o.topic.as_str());
        if o.true_payload.trim().is_empty()
            || o.false_payload.trim().is_empty()
            || o.true_payload.trim().eq_ignore_ascii_case("TOGGLE")
            || o.false_payload.trim().eq_ignore_ascii_case("TOGGLE")
        {
            return Err(
                "Output payloads must be non-empty explicit targets; TOGGLE is forbidden".into(),
            );
        }
        if o.state.is_some() && o.rules.is_some() {
            return Err("Output cannot supply both state and rules".into());
        }
        if o.device
            .as_ref()
            .is_some_and(|name| !c.output_devices.iter().any(|d| &d.name == name))
        {
            return Err("Output references an undeclared device".into());
        }
        let rules = o.rules();
        if rules.is_empty() {
            return Err("Output rules must be non-empty".into());
        }
        unique(rules.iter().map(|r| r.name.as_str()), "Rules")?;
        let mut priorities = BTreeSet::new();
        for r in rules {
            if !priorities.insert(r.priority) {
                return Err("Rule priorities must be distinct within each output".into());
            }
            if (r.start_on_match || !r.restart_on.is_empty()) && r.behavior.duration().is_none() {
                return Err("start_on_match and restart_on require a timed behavior".into());
            }
            let mut transitions = BTreeSet::new();
            for t in &r.restart_on {
                if t.from == t.to || !transitions.insert((t.state, t.from, t.to)) {
                    return Err("restart_on requires distinct true/false transitions".into());
                }
            }
            if let Behavior::SteadyFor {
                duration_seconds, ..
            } = r.behavior
            {
                seconds(
                    duration_seconds,
                    c.output_settings.min_hold_seconds,
                    "duration_seconds",
                )?;
            }
            if let Behavior::BlinkFor {
                interval_seconds,
                duration_seconds,
            } = r.behavior
            {
                seconds(
                    interval_seconds,
                    2. * c.output_settings.min_hold_seconds,
                    "interval_seconds",
                )?;
                seconds(duration_seconds, interval_seconds, "duration_seconds")?;
            }
        }
    }
    let h = &c.heartbeat;
    seconds(h.interval_seconds, 1., "heartbeat.interval_seconds")?;
    seconds(h.timeout_seconds, 1., "heartbeat.timeout_seconds")?;
    if h.interval_seconds.fract() != 0.
        || h.timeout_seconds.fract() != 0.
        || h.timeout_seconds > 65535.
        || h.timeout_seconds < 2. * h.interval_seconds
        || h.timeout_seconds
            <= h.interval_seconds
                + c.output_settings.min_hold_seconds
                + c.mqtt_settings.publish_timeout_seconds
                + c.runtime_settings.worker_stall_seconds
    {
        return Err("Heartbeat timing must use whole seconds and satisfy watchdog margins (timeout <= 65535)".into());
    }
    if h.enabled {
        let t = h
            .topic
            .as_deref()
            .ok_or("Enabled heartbeat requires topic")?;
        topic(t)?;
        if h.payload.as_ref().is_none_or(|p| p.trim().is_empty()) {
            return Err("Enabled heartbeat requires payload".into());
        }
        if !publish_topics.insert(t) {
            return Err("Heartbeat and relay topics must differ".into());
        }
    }
    if publish_topics
        .iter()
        .any(|t| subscriptions.contains_key(*t))
    {
        return Err("Publish topics cannot also be input or availability topics".into());
    }
    Ok(())
}
pub fn subscriptions(c: &Config) -> BTreeMap<String, Vec<String>> {
    let mut result: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut add = |t: &str, role: String| result.entry(t.into()).or_default().push(role);
    for (name, b) in c.inputs.booleans() {
        add(&b.topic, name.into());
    }
    for f in &c.inputs.faults {
        add(&f.mapping.topic, format!("fault:{}", f.name));
    }
    if let Some(b) = &c.inputs.battery {
        add(&b.topic, "battery_percent".into());
    }
    if let Some(l) = &c.inputs.location {
        match l {
            LocationInput::Json { topic, .. } => add(topic, "location".into()),
            LocationInput::Split {
                latitude_topic,
                longitude_topic,
                ..
            } => {
                add(latitude_topic, "latitude".into());
                add(longitude_topic, "longitude".into());
            }
        }
    }
    for d in &c.output_devices {
        add(&d.availability.topic, format!("device:{}", d.name));
    }
    result
}
fn unknown_keys(value: &Value, path: &str) -> Vec<String> {
    let allowed = match path {
        "" => "http mqtt_settings inputs state_settings outputs output_devices jitter output_settings runtime_settings telemetry_settings history_settings heartbeat logging",
        "secrets" => "mqtt_username mqtt_password mqtt_client_id http_state_token",
        "http" => "use_http interface port state_endpoint_enabled",
        "mqtt_settings" => "ip port use_tls validate_certs keep_alive_seconds qos retain_commands publish_retry_initial_seconds publish_retry_max_seconds publish_timeout_seconds",
        "inputs" => "location battery charging plugged_in charge_complete parked locked online source_healthy faults",
        "inputs.location" => if value["mode"] == "split" { "mode latitude_topic longitude_topic max_coordinate_skew_seconds stale_after_seconds" } else { "mode topic stale_after_seconds" },
        "inputs.battery" => "topic stale_after_seconds",
        "jitter" => "max_changes cooldown_seconds",
        "output_settings" => "min_hold_seconds",
        "runtime_settings" => "worker_stall_seconds",
        "telemetry_settings" => "timeout_seconds",
        "history_settings" => "max_entries",
        "heartbeat" => "enabled topic payload interval_seconds timeout_seconds",
        p if p.ends_with(".behavior") => match value["macro"].as_str() {
            Some("steady") => "macro value",
            Some("steady_for") => "macro value duration_seconds",
            _ => "macro interval_seconds duration_seconds",
        },
        p if p.starts_with("outputs[") && p.contains(".restart_on[") => "state from to",
        p if p.starts_with("outputs[") && p.contains(".rules[") => "name state when priority behavior start_on_match restart_on",
        p if p.starts_with("outputs[") => "name color topic device true_payload false_payload default_value state rules",
        p if p.starts_with("state_settings[") => "name target_latitude target_longitude inner_radius_meters outer_radius_meters battery_low_percent battery_low_is_fault",
        p if p.starts_with("output_devices[") && !p.ends_with(".availability") => "name availability",
        p if p.starts_with("inputs.faults[") => "name topic true_values false_values stale_after_seconds",
        p if p.starts_with("inputs.") || p.ends_with(".availability") => "topic true_values false_values stale_after_seconds",
        _ => return Vec::new(), // Logger owns its own schema, and extension content is ignored once.
    };
    let mut warnings = Vec::new();
    if let Some(object) = value.as_object() {
        for (key, v) in object {
            let child = if path.is_empty() {
                key.clone()
            } else {
                format!("{path}.{key}")
            };
            if !allowed.split_whitespace().any(|a| a == key) {
                warnings.push(format!("Unknown configuration key: {child}"));
            } else if let Some(array) = v.as_array() {
                for (i, entry) in array.iter().enumerate() {
                    warnings.extend(unknown_keys(entry, &format!("{child}[{i}]")));
                }
            } else if v.is_object() {
                warnings.extend(unknown_keys(v, &child));
            }
        }
    }
    warnings
}
