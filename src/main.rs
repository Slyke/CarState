use carstate::{
    app_log::{self, AppLog, Identity},
    config,
    engine::{Engine, SnapshotIdentity},
    http::{self, HttpState},
    model::{BuildInfo, Clock},
    runtime::Runtime,
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    process::ExitCode,
    sync::{atomic::AtomicBool, Arc, RwLock},
    time::Duration,
};
use styleguide_logger::{ErrorOptions, LogOptions, Logger};
use tokio::sync::watch;

#[tokio::main]
async fn main() -> ExitCode {
    let bootstrap = app_log::bootstrap();
    match run(&bootstrap).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(reason) => {
            bootstrap
                .wrap_error(ErrorOptions {
                    caller: "carstate::main".into(),
                    reason,
                    error_key: "STARTUP_OR_RUNTIME_FAILED".into(),
                    ..Default::default()
                })
                .await;
            ExitCode::FAILURE
        }
    }
}
async fn run(bootstrap: &Logger) -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        bootstrap.generate_log(LogOptions {level:"info".into(),caller:"carstate::cli".into(),logger_key:Some("CLI_HELP".into()),message:"carstate [--validate-config | --dry-run]\n--validate-config: validate config/secrets/logging and exit; no network, listeners, output, or file mutations.\n--dry-run: observe MQTT and simulate decisions with a fresh diagnostic ID; no PUBLISH or Last Will, cannot maintain a watchdog.\nCARSTATE_DRY_RUN=true|false: select observation or normal publishing; defaults to false. --dry-run always enables observation; --validate-config ignores this variable.\nNormal mode publishes relay commands. Production requires one active writer per output set; stop and confirm termination before replacement. Config changes require restart.".into(),..Default::default()}).await;
        return Ok(());
    }
    let validate = args.iter().any(|a| a == "--validate-config");
    let dry = args.iter().any(|a| a == "--dry-run");
    if (validate && dry)
        || args
            .iter()
            .any(|a| a != "--validate-config" && a != "--dry-run")
    {
        return Err("Use only one of --validate-config or --dry-run; see --help".into());
    }
    let loaded = config::load()?;
    if validate {
        for warning in &loaded.warnings {
            bootstrap
                .generate_log(LogOptions {
                    level: "warn".into(),
                    caller: "carstate::validation".into(),
                    logger_key: Some("CONFIG_VALIDATION_WARNING".into()),
                    message: warning.clone(),
                    ..Default::default()
                })
                .await;
        }
        bootstrap.generate_log(LogOptions {level:"info".into(),caller:"carstate::validation".into(),logger_key:Some("CONFIG_VALIDATED".into()),message:"Configuration, secrets, logging and error catalog are valid; no network or publishing started".into(),..Default::default()}).await;
        return Ok(());
    }
    let env_dry = match std::env::var("CARSTATE_DRY_RUN") {
        Ok(value) => value
            .parse::<bool>()
            .map_err(|_| "CARSTATE_DRY_RUN must be true or false")?,
        Err(std::env::VarError::NotPresent) => false,
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err("CARSTATE_DRY_RUN must be true or false".into());
        }
    };
    let dry = dry || env_dry;
    let (id, generated) = app_log::generate_id(&loaded.secrets.mqtt_client_id, dry)?;
    let env: BTreeMap<_, _> = std::env::vars().collect();
    let identity = Identity::new(&id, &env);
    let clock = Clock::default();
    let build = BuildInfo::default();
    let c = &loaded.config;
    let l = &loaded.logging;
    let sink_summary = json!({"console":{"enabled":l.sinks.console.enabled,"format":l.sinks.console.format},"stdout":{"enabled":l.sinks.stdout.enabled,"format":l.sinks.stdout.format},"stderr":{"enabled":l.sinks.stderr.enabled,"format":l.sinks.stderr.format},"file":{"enabled":l.sinks.file.enabled,"format":l.sinks.file.format},"http":{"enabled":l.sinks.http.enabled,"format":l.sinks.http.format},"syslog":{"enabled":l.sinks.syslog.enabled,"format":l.sinks.syslog.format}});
    let mut context = json!({"service":"carstate","version":build.version,"buildHash":build.build_hash,"startedAt":clock.stamp(Some(0.)),"platform":std::env::consts::OS,"architecture":std::env::consts::ARCH,"configPath":loaded.config_path,"secretsPath":loaded.secrets_path,"mqttClientId":id,"mqttClientIdGenerated":generated,"brokerHost":c.mqtt_settings.ip,"brokerPort":c.mqtt_settings.port,"tlsEnabled":c.mqtt_settings.use_tls,"validateCertificates":c.mqtt_settings.validate_certs,"inputCount":config::subscriptions(c).values().flatten().filter(|r|!r.starts_with("device:")).count(),"stateSettingsCount":c.state_settings.len(),"outputCount":c.outputs.len(),"httpEnabled":c.http.use_http,"stateEndpointEnabled":c.http.state_endpoint_enabled,"httpInterface":c.http.interface,"httpPort":c.http.port,"stateTokenConfigured":!loaded.secrets.http_state_token.trim().is_empty(),"mqttCredentialsConfigured":!loaded.secrets.mqtt_username.is_empty() || !loaded.secrets.mqtt_password.is_empty(),"logSinks":sink_summary,"kubernetesAttached":l.kubernetes.enabled,"jitter":c.jitter,"relayHoldSeconds":c.output_settings.min_hold_seconds,"retryInitialSeconds":c.mqtt_settings.publish_retry_initial_seconds,"retryMaxSeconds":c.mqtt_settings.publish_retry_max_seconds,"publishTimeoutSeconds":c.mqtt_settings.publish_timeout_seconds,"workerStallSeconds":c.runtime_settings.worker_stall_seconds,"heartbeat":{"enabled":c.heartbeat.enabled,"topic":c.heartbeat.topic,"intervalSeconds":c.heartbeat.interval_seconds,"expectedTimeoutSeconds":c.heartbeat.timeout_seconds,"watchdogVerification":"not_verified"},"mode":if dry {"dry_run"} else {"normal"},"publishingEnabled":!dry,"singleWriterRequirement":"One active writer per physical output set; operational assumption, no distributed lock","behaviors":c.outputs.iter().flat_map(|o|o.rules().into_iter().map(|r|json!({"output":o.name,"rule":r.name,"behavior":r.behavior}))).collect::<Vec<_>>(),"freshness":c.inputs.booleans().iter().map(|(n,m)|json!({"input":n,"staleAfterSeconds":m.stale_after_seconds})).collect::<Vec<_>>(),"locationStaleAfterSeconds":c.inputs.location.as_ref().and_then(config::LocationInput::stale)});
    if let Some(number) = &build.build_number {
        context["buildNumber"] = json!(number);
    }
    if !l.kubernetes.enabled {
        context["kubernetes"] = app_log::kubernetes(&env);
    }
    let logger = Logger::new(loaded.logging, loaded.codes)
        .map_err(|_| "Cannot initialize configured logging transports")?;
    let outcome = logger
        .generate_log(LogOptions {
            level: "info".into(),
            caller: "carstate::main".into(),
            logger_key: Some("SERVICE_BOOT_DIAGNOSTICS".into()),
            message: "Carstate boot diagnostics".into(),
            context: Some(identity.enrich(context)),
            ..Default::default()
        })
        .await;
    if !outcome.failures.is_empty() {
        bootstrap
            .generate_log(LogOptions {
                level: "warn".into(),
                caller: "carstate::main".into(),
                logger_key: Some("BOOT_LOG_DELIVERY_FAILED".into()),
                message: "One or more startup log sinks failed".into(),
                ..Default::default()
            })
            .await;
    }
    let (log, mut logging_worker) = AppLog::start(logger, identity);
    for warning in loaded
        .warnings
        .iter()
        .filter(|s| !s.starts_with("Blank mqtt_client_id"))
    {
        log.emit("warn", "CONFIG_UNKNOWN_OR_UNAVAILABLE", warning, json!({}));
    }
    if generated {
        log.emit(
            "warn",
            if dry {
                "DRY_RUN_CLIENT_ID"
            } else {
                "MQTT_CLIENT_ID_GENERATED"
            },
            if dry {
                "Dry-run overrides the configured ID with a fresh diagnostic identity"
            } else {
                "Generated MQTT identity; set mqtt_client_id for a stable identity"
            },
            json!({"client_id":id}),
        );
    }
    if c.mqtt_settings.use_tls && !c.mqtt_settings.validate_certs {
        log.emit(
            "warn",
            "MQTT_TLS_UNVERIFIED",
            "MQTT certificate trust and hostname verification disabled by configuration",
            json!({}),
        );
    }
    let snapshot_identity = SnapshotIdentity {
        build: build.clone(),
        client_id: id,
        generated,
        credentials: !loaded.secrets.mqtt_username.is_empty()
            || !loaded.secrets.mqtt_password.is_empty(),
    };
    let mut initial = Engine::new(c.clone(), dry);
    initial.tick(0., |_| unreachable!("not connected"));
    let shared = Arc::new(RwLock::new(initial.snapshot(
        0.,
        &clock,
        &snapshot_identity,
    )));
    let workers_ok = Arc::new(AtomicBool::new(true));
    let state = HttpState {
        snapshot: shared.clone(),
        build,
        state_token: Arc::new(loaded.secrets.http_state_token.clone()),
        state_enabled: c.http.state_endpoint_enabled,
        workers_ok: workers_ok.clone(),
        log: log.clone(),
    };
    let listener = if c.http.use_http {
        Some(
            tokio::net::TcpListener::bind((c.http.interface.as_str(), c.http.port))
                .await
                .map_err(|_| "Cannot bind configured HTTP listener")?,
        )
    } else {
        None
    };
    let (stop_tx, stop_rx) = watch::channel(false);
    let mut http_worker = if let Some(listener) = listener {
        log.emit(
            "info",
            "HTTP_LISTENING",
            "HTTP listener bound",
            json!({"interface":c.http.interface,"port":c.http.port}),
        );
        let mut stop = stop_rx.clone();
        Some(tokio::spawn(async move {
            axum::serve(listener, http::router(state))
                .with_graceful_shutdown(async move {
                    let _ = stop.changed().await;
                })
                .await
        }))
    } else {
        drop(state);
        log.emit("info", "HTTP_DISABLED", "HTTP listener disabled", json!({}));
        None
    };
    let runtime = Runtime {
        config: loaded.config,
        secrets: loaded.secrets,
        identity: snapshot_identity,
        clock,
        dry,
        snapshot: shared,
        workers_ok,
        log: log.clone(),
    };
    let mut controller = tokio::spawn(runtime.run(stop_rx));
    let mut controller_done = false;
    let result = tokio::select! {
        signal=shutdown_signal()=>signal,
        value=&mut controller=>{controller_done=true;match value {Ok(Ok(()))=>Err("Controller exited unexpectedly".into()),Ok(Err(e))=>Err(e),Err(_)=>Err("Controller task failed".into())}},
        _=async {if let Some(h)=&mut http_worker {let _=h.await;} else {std::future::pending::<()>().await}}=>Err("HTTP worker exited unexpectedly".into()),
        _=&mut logging_worker=>Err("Logging worker exited unexpectedly".into()),
    };
    let _ = stop_tx.send(true);
    log.emit(
        "info",
        "SERVICE_SHUTDOWN",
        "Stopping relay commands and heartbeats; device watchdog owns fallback",
        json!({}),
    );
    if !controller_done
        && tokio::time::timeout(Duration::from_secs(5), &mut controller)
            .await
            .is_err()
    {
        controller.abort();
    }
    if let Some(mut h) = http_worker {
        if !h.is_finished()
            && tokio::time::timeout(Duration::from_secs(5), &mut h)
                .await
                .is_err()
        {
            h.abort();
        }
    }
    drop(log);
    if !logging_worker.is_finished()
        && tokio::time::timeout(Duration::from_secs(5), &mut logging_worker)
            .await
            .is_err()
    {
        logging_worker.abort();
    }
    result
}
async fn shutdown_signal() -> Result<(), String> {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .map_err(|_| "Cannot register SIGTERM handler")?;
        tokio::select! {_=tokio::signal::ctrl_c()=>(),_=term.recv()=>()};
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c()
        .await
        .map_err(|_| "Cannot register interrupt handler")?;
    Ok(())
}
