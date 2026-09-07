# Carstate

Carstate is a Rust service for one vehicle and one MQTT broker. It turns configurable vehicle telemetry into boolean application states and explicit Tasmota relay commands. It does not call a vehicle API. An upstream publisher supplies the facts.

The implementation follows [IMPLEMENTATION_SPEC.md](./IMPLEMENTATION_SPEC.md) and the sibling styleguide's configuration, deployment and logging contracts. There is no JavaScript runtime, UI, database, or persistent state.

## Quick start

Install Rust 1.98.1 and run commands from this repository's root:

```sh
cp ./config/secrets.example.json5 ./config/secrets.json5
chmod 600 ./config/secrets.json5
# Edit ./config/carstate.json5 and ./config/secrets.json5 for your broker and topics.
cargo run --locked -- --validate-config
cargo run --locked -- --dry-run
# After reviewing the hypothetical commands and establishing sole-writer ownership:
cargo run --locked
```

`--validate-config` loads and validates the same configuration, secrets, environment references, logger transport settings and error catalog as normal startup. It opens no sockets or HTTP listeners, creates no log files or IDs, and exits nonzero for invalid settings. Unknown configuration keys produce warnings with their exact paths.

`--dry-run` subscribes and runs the full state, freshness, jitter, timer and hold pipeline with a recording sink. It always uses a fresh `carstate_dryrun_XXXXXXXX` identity, even if a fixed live ID is configured. It sends **no MQTT PUBLISH packets or Last Will**, including heartbeats. Structured `would_publish` and `would_heartbeat` events show hypothetical decisions. Real submission fields and counters stay zero. Dry-run cannot maintain a device watchdog.

The two flags are mutually exclusive. `--help` documents their side effects. Configuration changes require a restart.

## Docker development

Requires Docker Compose 2.24 or later. The default Compose project starts an isolated, anonymous development Mosquitto broker and a Rust development container. Published ports bind to localhost. The application defaults to dry-run and uses [carstate.dev.json5](./config/carstate.dev.json5), whose broker host is `mqtt`.

```sh
docker compose up --build
# In another terminal:
docker compose exec carstate cargo test --locked
docker compose exec carstate cargo clippy --locked --all-targets -- -D warnings
docker compose restart carstate
# Stop the development services:
docker compose down
```

Source is bind-mounted, and named volumes cache Cargo registry, Git dependencies and build output. Restart the application after editing Rust files; its `cargo run` command rebuilds as needed. To test normal publishing, use `docker compose run --rm --service-ports carstate cargo run --locked` only after stopping the existing application container. Keep this configuration on its isolated development broker.

The sample light has availability tracking. Readiness remains false until its `Online` message arrives. Seed example inputs on the development broker:

```sh
docker compose exec mqtt mosquitto_pub -t tele/garage-light/LWT -m Online -r
docker compose exec mqtt mosquitto_pub -t vehicle/location -m '{"latitude":49.2827,"longitude":-123.1207}'
docker compose exec mqtt mosquitto_pub -t vehicle/parked -m true
docker compose exec mqtt mosquitto_pub -t vehicle/battery_level -m 80
docker compose exec mqtt mosquitto_pub -t vehicle/fault -m healthy
docker compose exec mqtt mosquitto_pub -t vehicle/tpms_soft_warning_fl -m false
docker compose exec mqtt mosquitto_pub -t vehicle/charging_state -m Complete
# Completion keeps green on and the plug-in reminder off. Disconnection starts orange:
docker compose exec mqtt mosquitto_pub -t vehicle/charging_state -m Disconnected
```

These commands address only the example development broker. They do not install device rules. The other optional vehicle facts may remain unknown without failing readiness.

Both Compose files inject a root `.env` when present and work without one. Compose interpolation alone would not inject arbitrary variables; each application service therefore declares `env_file` with `required: false`. `.env` and real secrets are ignored by Git and excluded from Docker's build context. The development default uses the committed anonymous secrets example; set `CARSTATE_SECRETS_PATH` in `.env` to use another file.

## Configuration

[carstate.json5](./config/carstate.json5) is a full four-color example. Adjust the geofences and upstream value semantics before normal operation. Every input is optional, but at least one vehicle input and one output are required. Exact topics only: MQTT wildcards, NULs, duplicate output topics, and publish/input topic collisions are rejected.

| Setting | Default / behavior |
| --- | --- |
| `http.use_http`, `interface`, `port` | `true`, `0.0.0.0`, `3000` |
| `http.state_endpoint_enabled` | `false`; enabling requires HTTP and a nonblank bearer token |
| `mqtt_settings.qos`, `retain_commands` | `1`, `false`; only QoS 0/1 supported |
| `mqtt_settings.use_tls`, `validate_certs` | `false`, `true`; TLS uses system trust roots |
| `mqtt_settings.keep_alive_seconds` | `30` |
| `jitter.max_changes`, `cooldown_seconds` | Three admitted changes, then a full ten-second cooldown |
| `output_settings.min_hold_seconds` | `1`; never below one second |
| `publish_retry_initial_seconds`, `publish_retry_max_seconds` | `1`, `30`, under `mqtt_settings` |
| `mqtt_settings.publish_timeout_seconds` | `5`; submissions use nonblocking acceptance and stalled dispatch/session waits are bounded |
| `runtime_settings.worker_stall_seconds` | `30`; idle-capable progress checks run at least once per second |
| `stale_after_seconds` on an input | Null/omitted disables expiry |
| Split GPS `max_coordinate_skew_seconds` | `10`; each fix requires both axes to be republished |
| `state_settings`, `output_devices` | Empty arrays |
| `heartbeat.enabled`, `interval_seconds`, `timeout_seconds` | `false`, `120`, `360` |

Configuration and secrets use JSON5. A whole string `${ENV_VAR}` resolves recursively to the environment value, preserving it exactly as a string. Unset variables become null; defined empty variables stay empty strings. Partial references and object keys are unchanged. Numeric and boolean settings require those JSON5 types; they are not inferred from environment strings. HTTP port additionally accepts a numeric string for compatibility.

[secrets.env.example.json5](./config/secrets.env.example.json5) illustrates environment-backed credentials. Supply all four variables when using it; missing variables become null and fail the string schema. Explicit empty username/password strings support anonymous brokers. No application code loads `.env` directly; Compose or the process launcher injects it.

Supported direct overrides, applied after reference expansion:

- `CARSTATE_CONFIG_PATH`: defaults to `./config/carstate.json5`.
- `CARSTATE_SECRETS_PATH`: defaults to `./config/secrets.json5`.
- `CARSTATE_HTTP_PORT`, `CARSTATE_USE_HTTP`, `CARSTATE_HTTP_INT`: retained compatibility overrides.

A missing/blank `mqtt_client_id` generates eight random uppercase letters/digits from the OS once per process, warns once, and reuses that identity across reconnects. A configured nonblank ID is preserved exactly. IDs are never written back to disk. Client IDs do not provide exclusive controller ownership.

## States and outputs

The fixed Rust state enum covers geofence membership, charging, connection, completion, parked/locked/online/source health, battery-low and configured vehicle faults, plus the documented location composites. See the [complete state table](./IMPLEMENTATION_SPEC.md#fixed-application-states).

State values are true, false, or unknown. Unknown does not mean false. Malformed or unrecognized payloads preserve the last valid value. Shared topics update all their configured decoders atomically. `charging`, `plugged_in`, and `charge_complete` remain independent: reaching a charge target does not imply unplugging. `parked` is never inferred from GPS, inactivity or locking. Source/logger health is independent of vehicle faults and control readiness.

Evaluate each complete geofence/battery condition within its own named `state_settings` entry, then OR applicable entries. Inner/outer overlap across homes is allowed. An unchanged combined state during a home-to-home handoff does not restart a reminder or republish a steady output. Battery thresholds are independent of GPS, and each entry's `battery_low_is_fault` flag applies only to that entry's threshold.

Outputs have distinct names/topics and either a state shorthand or a nonempty rule list. Distinct integer priorities choose one winner per output. `when` defaults to true. With no matching rule, use `default_value` if at least one trigger is known; all-unknown rules stay silent. Macros:

- `steady`: holds its configured boolean value.
- `blink_for`: starts ON, with `interval_seconds` defining a complete ON/OFF cycle and `duration_seconds` bounding the episode. The interval must be at least twice the relay hold and duration at least one full interval. Expiry makes the rule ineligible until an admitted known non-match rearms it.

The example uses red for vehicle faults; orange for a bounded parked/unplugged reminder followed by steady orange; green for home and connected, including completion; and blue for the outer band. High-priority fault rules hold the other colors off. Wiring and payloads are configurable; `TOGGLE` is rejected.

Jitter filtering operates on each combined state, separately from raw diagnostics. Its first known value is immediate. After three transitions, later changes wait ten full seconds from the third transition; only the latest value remains pending. Unknown propagates immediately and never rearms an exhausted episode. Blink phases do not consume this budget.

Every relay command uses one scheduler: initial values, changes, phases, retries, preemption, defaults and resynchronization all observe the hold. Recovery also waits a full hold interval. The first eligible blink attempt anchors its duration, even if that attempt fails. Outages and preemption do not extend it. Delayed phases are skipped; there is no catch-up burst.

Successful submission means the MQTT client accepted the command, not broker acknowledgement or physical relay confirmation. Failed submissions retry without new telemetry and always reevaluate the current decision. Disconnect discards the entire old client/event-loop generation, including unsent relay and heartbeat buffers, before creating a clean session with the same ID. Already transmitted QoS 1 work may be duplicated; commands are idempotent explicit targets.

Freshness is based on **valid receipt time**, including retained messages. It cannot establish the upstream measurement's actual age. Choose suitable reporting cadences; expiry is disabled unless configured. Split GPS needs new valid latitude and longitude after each committed pair. An incomplete assembly has a non-extending skew deadline; one axis cannot keep an old location alive indefinitely. Prefer atomic JSON GPS when both axes cannot be published for every fix.

## HTTP and logging

`GET /livez` is dependency-free and returns 200 while the listener responds. `GET /readyz` returns 200 only for a healthy control pipeline, otherwise 500: MQTT connected, all subscriptions acknowledged, configured output devices online, workers progressing, and no unresolved submission failure. Recovery continues while unready. Sleeping cars and unavailable optional vehicle facts do not fail readiness. There is no `/healthz` alias.

Both probes return service/version/`buildHash`, `ok`, and correlation metadata without exposing broker addresses, client IDs, topics, counts or credentials. Canonical UUID `x-correlation-id` values are echoed; absent/invalid IDs are replaced. The same ID appears in the response header and JSON `correlation_id`.

`GET /state` is unregistered by default (404). When enabled it requires `Authorization: Bearer <http_state_token>`, returns JSON and `Cache-Control: no-store`, and exposes a consistent snapshot even before telemetry or during broker outages. Missing/invalid credentials return generic 401 JSON with `WWW-Authenticate: Bearer`. The snapshot includes timestamped facts, stale/unknown reasons, GPS assembly, raw/control/per-entry states, device status, output episodes/retries, heartbeat, connection/subscriptions, counters and build/runtime metadata. It is read-only and contains no usernames, passwords or bearer tokens.

All application events and structured errors use the bundled [Rust logger](./src/logger/README.md) and [application error catalog](./config/errors.json5). JSON console logs include startup build metadata with HTTP on or off. A bounded 1024-event worker keeps remote logging I/O out of the MQTT/control loop; overflow and sink failures are reported through a separate minimal custom console logger. Shutdown drains accepted log work for up to five seconds.

`INSTANCE_ID` takes precedence over `K8S_POD_NAME`, then `HOSTNAME`, then the effective MQTT ID. Application events/errors consistently include this identity, PID and available hostname. Populated `K8S_*` metadata automatically enables native logger enrichment unless `LOG_K8S_METADATA_ENABLED` or `logging.kubernetes.enabled` explicitly chooses otherwise. File settings override legacy `LOG_*` settings. Deliberately disabled enrichment is still summarized once at startup.

The logger supports console/stdout/stderr, file, HTTP and syslog sinks. Configure only those needed. `OUTPUT_BLINK_PHASE` is disabled in the shipped gate; successful heartbeats and frequent state/filter updates are debug events. Startup info and warnings are enabled. The copied logger adds a read-only `validate_transport_settings` API so validation mode can check headers/TLS files without initializing remote clients.

```sh
cargo error-add --error-file ./config/errors.json5 --error-key MY_NEW_ERROR --deterministic
cargo error-validate --error-file ./config/errors.json5
cargo logger-test --locked
```

## Production image and operation

The multi-stage Dockerfile has `development`, `builder`, and `production` targets. It installs the pinned Rust toolchain over the [official Rust image](https://hub.docker.com/_/rust/) and builds with the lockfile. The final Debian slim image contains the application, CA certificates, curl and example non-secret configuration. It runs as UID/GID `10001`, with no compiler, source tree, `.git`, `.env`, or secrets. Build metadata is generated by Rust's build script and embedded in the executable; handlers never run Git.

```sh
# Local image build; uncommitted/non-release builds may use unknown metadata.
docker build --target production --build-arg BUILD_HASH="$(git rev-parse --short=12 HEAD)" -t carstate:local .
# Validate mounted production settings before replacing the running instance.
docker compose -f ./compose.prod.yml run --rm --no-deps carstate --validate-config
# Start only after the sole-writer upgrade checklist below.
docker compose -f ./compose.prod.yml up -d --no-build
docker compose -f ./compose.prod.yml logs -f carstate
```

Prepare `./config/secrets.json5` and production config first. The container's UID/GID 10001 must be able to read them: for example, make the secrets file owned by the deployment operator with group 10001 and mode 0640, and ensure its parent directories are searchable. Keep non-secret `./config/errors.json5` readable (0644). Do not make secrets world-readable. A production orchestrator can mount its own secret file or inject variables into an environment-reference secrets file. The Compose file mounts `./config` read-only, drops capabilities, disables privilege escalation and provides only a small writable `/tmp`. File logging needs a separately writable mounted destination.

Set `CARSTATE_IMAGE` to an immutable published tag. Published HTTP defaults to localhost port 3000. Adjust the mapping or put the listener behind your trusted proxy as appropriate. If HTTP is disabled or its configured port changes, adapt external probe configuration accordingly; the image does not hard-code a healthcheck that would contradict those supported settings.

On Ctrl+C/SIGTERM, Carstate stops new commands and heartbeats and cancels MQTT/timer work. It sends no all-off shutdown command. The installed local watchdog owns fallback. Already accepted network work cannot be recalled.

### Sole-writer upgrade checklist

1. Reserve relay and heartbeat topics for exactly one active controller. Exclude competing automation/manual writers during normal operation.
2. Validate the new configuration/image, using dry-run with its separate diagnostic ID if helpful.
3. Stop the old controller: `docker compose -f ./compose.prod.yml stop carstate`. Confirm its process has actually terminated and no other writer remains.
4. Start the replacement with the chosen immutable tag. Never overlap old/new containers, blue-green deployments or autoscaling replicas.
5. Check startup version/hash, readiness/subscription/device recovery, and current relay restoration. Keep the previous immutable tag for rollback using the same stop/confirm/start procedure.

A Kubernetes deployment must use one replica and `strategy: { type: Recreate }`, with no HPA. Recreate is an operational aid, not fencing: if an old node/process is partitioned or terminating, prove the writer stopped before replacement. Do not treat a shared MQTT ID as a lock. Configure `/livez` and `/readyz` on the chosen HTTP port and a termination grace period of at least 20 seconds. Populate `K8S_POD_NAME`, `K8S_NAMESPACE`, `K8S_POD_IP`, `K8S_NODE_NAME` via the Downward API and set `K8S_DEPLOYMENT` explicitly. Use `app.kubernetes.io/version` and a build-hash annotation matching the image.

### Manual Tasmota watchdog setup

Heartbeat is disabled by default. Enable it only after installing and testing a matching device-side watchdog. The example uses topic `cmnd/garage-light/Event`, payload `carstate_heartbeat=alive`, interval 120 seconds and expected timeout 360 seconds. Pulses use QoS 0 and are never retained. Every due pulse first resynchronizes current known relay commands through the normal hold/retry scheduler. Failed workers, unavailable devices/broker or failed delivery suppress pulses; heartbeat-specific failures retry through a freshly authorized recovery pass.

For an **unused** rule/timer slot 1, the example all-off fallback is:

```text
Rule1 ON System#Init DO RuleTimer1 360 ENDON ON Event#carstate_heartbeat=alive DO RuleTimer1 360 ENDON ON Rules#Timer=1 DO Backlog Power1 OFF; Power2 OFF; Power3 OFF; Power4 OFF ENDON
Rule1 4
Rule1 1
RuleTimer1 360
```

Inspect existing rules/timer slots and merge deliberately; never overwrite an occupied slot. `System#Init` arms fallback before networking, repeated matching events reset the timer, and installation arms it immediately. Set every `360` to your configured timeout. Firmware must support Rules. An arbitrary heartbeat topic needs an appropriate device subscription/bridge; the Event command-topic approach avoids that extra feature. See [Tasmota Rules](https://tasmota.github.io/docs/Rules/) and [rule commands](https://tasmota.github.io/docs/Commands/#rules).

Operator verification on a dedicated test device:

1. Choose fallback relay states and inspect for conflicting rules and old retained relay/heartbeat messages. Carstate never installs rules or clears retained data automatically.
2. Install/enable the watchdog, then enable matching Carstate heartbeat settings.
3. Stop the controller and verify timeout fallback. Repeat with the broker unreachable, then with the device rebooting while the broker is unavailable.
4. Restore the controller/network and verify that current known outputs are resynchronized. Confirm periodic resynchronization restores a watchdog fallback even without a connection event.
5. If the physical relay must never switch faster than once per second, install matching device-side hold protection and measure it separately. Carstate guarantees spacing of its own submissions; network buffering/retransmission, competing clients, power cycles and local watchdog actions are outside that guarantee.

The expected timeout must be a whole number of seconds, at least twice the interval, greater than interval + relay hold + publish timeout + worker-stall allowance, and at most 65535 for the example RuleTimer. Carstate reports `watchdog_verification: "not_verified"`: software tests do not prove that your live device's watchdog exists or works during power/firmware failure.

## Publishing images

`Cargo.toml` is the canonical SemVer version. Publish the matching `vVERSION` Git tag from a clean committed tree; the current source must have a real commit before a release. Do not force-move deployed version tags.

```sh
VERSION="v$(sed -n 's/^version = "\([^"]*\)"/\1/p' ./Cargo.toml | head -n 1)"
git status --short
git tag -a "$VERSION" -m "$VERSION"
git push origin "$VERSION"
# Authenticate to Docker Hub and your second registry using your normal credential flow.
USERNAME=YOURUSERNAME DOMAIN=registry.example.com ./scripts/publish-images.sh
```

The [publishing script](./scripts/publish-images.sh) verifies a clean tree and that the version tag points to HEAD, runs Rust checks/tests, passes a twelve-character Git hash with release validation to Docker, then tags and pushes the **same image** to both registries as:

- `latest`
- `v0.1.0`
- `v0.1.0-<12-character-Git-hash>`

Replace the illustrative version by the manifest version. Prefer the version-plus-hash tag for deployment and rollback. `CARSTATE_RELEASE=true` builds reject missing/invalid commit hashes; normal local builds can use `BUILD_HASH` or `unknown`. Optional `BUILD_NUMBER` is included only when supplied. Runtime logs, probes and state diagnostics all reuse the same embedded metadata. No images are automatically published by the check workflow.

## Verification and code layout

```sh
cargo fmt --all -- --check
cargo check --locked
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo logger-test --locked
cargo logger-check
cargo error-validate --error-file ./config/errors.json5
docker compose config --quiet
docker compose -f ./compose.prod.yml config --quiet
```

Tests use injected monotonic time, recording/failing sinks and an ephemeral local MQTT wire simulator. They cover shared-topic parsing, every fixed state, optional inputs, multiple homes, timestamp/freshness/coherence behavior, jitter, blink expiry/rearming/preemption, holds/retries, device and heartbeat recovery, non-publishing dry-run, config-only side effects, auth/probes, identity/logging and MQTT `POWER1`–`POWER4` packets. They never connect to configured physical relays.

- `src/config.rs`, `src/config_value.rs`: schema, environment/default processing and validation.
- `src/model.rs`, `src/inputs.rs`, `src/rules.rs`: facts, timestamps and pure state evaluation.
- `src/behaviors.rs`, `src/outputs.rs`, `src/engine.rs`: filters, episodes, command scheduling and heartbeat authorization.
- `src/mqtt.rs`, `src/runtime.rs`: bounded MQTT I/O, connection generations and supervised control loop.
- `src/http.rs`, `src/app_log.rs`, `src/main.rs`: diagnostics, custom logging, startup and graceful shutdown.

The evaluator, timers, publisher decisions and heartbeat run serially in one controller task with no blocking network/logging work. This intentionally combines the spec's suggested worker roles so snapshot updates are atomic. An independent progress monitor supervises that task, and MQTT polling reports idle-capable progress separately. A single sleeping task handle is never the readiness check.
