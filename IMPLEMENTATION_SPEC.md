# Carstate implementation specification

Status: implementation-ready MVP specification

## Goal

Carstate is a small, long-running Rust service for one vehicle. It consumes vehicle facts from configurable MQTT topics, normalizes them into an in-memory state, evaluates a fixed set of boolean application states, and maps those states to configurable MQTT commands for Tasmota outputs.

Carstate is vehicle-vendor neutral. It does not call Tesla, TeslaMate, or any other vehicle API directly. An upstream system is responsible for publishing the source facts.

## Scope

The application handles exactly one car and one MQTT broker, with an array of state-setting entries (for example, multiple homes, each with its own geofence and battery settings). It tracks these facts when configured:

- GPS latitude and longitude
- Charging state
- Plugged-in state
- Charge completion, when reported by the source
- Parked state, when reported by the source
- Lock state
- Battery level
- Online/offline state
- Upstream source/logger health (`source_healthy`)
- Optional named fault/warning signals, such as a reported vehicle fault or per-wheel TPMS warnings

None of these individual facts are mandatory. Carstate subscribes only to inputs present in configuration and ignores unsupported or undefined data. For example, a deployment with no TPMS topics must run normally without TPMS state.

Each configured fact may arrive independently and at a different time. Carstate keeps the most recent valid value for each fact and reevaluates affected application states after every update or freshness deadline. Raw facts and derived states remain observable immediately; a separate jitter filter admits the control-state changes used by output rules.

## Processing flow

1. Initialize the custom bootstrap logger, load/validate configuration and secrets, configure the shared custom logger, and emit startup diagnostics before normal processing.
2. Connect to MQTT.
3. Subscribe to configured vehicle-input and optional output-device availability topics, deduplicating shared topics and tracking subscription acknowledgements.
4. Parse incoming messages into normalized vehicle facts.
5. Evaluate affected states for each applicable `state_settings` entry and OR the results per state. Direct vehicle facts remain shared across entries.
6. Pass combined states through the configurable jitter filter, select the highest-priority matching rule for each output, and run its configured built-in behavior. Submit only the latest desired relay command, subject to the minimum hold time.
7. Retry failed submissions without waiting for new vehicle data, resubscribe after reconnecting, and resynchronize known outputs on connection/device recovery. When enabled, run the heartbeat/watchdog contract below.
8. Optionally serve HTTP liveness and readiness endpoints.
   Expose a JSON state snapshot only when its separate, default-off setting is enabled.
9. Shut down cleanly on Ctrl+C or a normal termination signal: stop heartbeats and new relay commands, cancel timers/retries, and drain custom-logger work with a bounded wait. The configured Tasmota-side watchdog owns fallback; Carstate does not send an all-off shutdown command.

## Normalized vehicle facts

Use one in-memory state structure similar to:

```rust
struct VehicleState {
    location: Option<Location>,
    charging: Option<bool>,
    plugged_in: Option<bool>,
    charge_complete: Option<bool>,
    parked: Option<bool>,
    locked: Option<bool>,
    battery_percent: Option<f64>,
    online: Option<bool>,
    source_healthy: Option<bool>,
    faults: HashMap<String, Option<bool>>,
}
```

`None` means that an optional fact was not configured, no valid value has been received, or its optional freshness/coherence policy has made it unknown. Malformed or unrecognized payloads for configured topics must be logged and ignored; they must not erase a previous valid value. Unknown JSON fields and unknown MQTT topics must be ignored.

### GPS input

Support either of these mutually exclusive modes.

JSON mode uses one topic and accepts an object containing numeric coordinates:

```json
{
  "latitude": 49.8427,
  "longitude": -123.4257
}
```

Also accept `lat` and `lng` as aliases. Ignore extra fields.

Split mode uses one latitude topic and one longitude topic. Each payload is a UTF-8 number such as `49.8427` or `-123.4257`. Store component values, valid-receipt times, and update generations independently. Publish a normalized location only from a completed pair under the split-coordinate coherence policy below.

Latitude must be from `-90` through `90`; longitude must be from `-180` through `180`. Reject non-finite values.

### Optional freshness and split-coordinate coherence

Every input mapping, including each named fault and the location input, may set `stale_after_seconds`: a finite positive number, or null/omitted to disable expiry. No manufacturer-specific timeout is inferred.

- Track per-input `last_received_at` for valid, recognized updates separately from `last_changed_at`. A valid duplicate refreshes freshness without advancing the value-change timestamp; malformed/unrecognized traffic does neither. MQTT transport counters still count rejected messages separately.
- Use a monotonic deadline from the valid receipt time. At age greater than or equal to the configured timeout, make the effective fact unknown and reevaluate states, control filters, and outputs without waiting for another message. Preserve the last known value/time for diagnostics. Fresh data restores a known fact even when its value equals the expired one.
- Record an explicit unknown reason: `not_configured`, `never_received`, `stale`, or `incoherent`, as applicable. Omitted inputs remain optional, and stale/unknown facts alone do not fail Carstate readiness or stop its process heartbeat.
- Unknown is not false. Expiry cannot clear/rearm an exhausted reminder. Unknown dependent output triggers suspend their behavior under the normal unknown-state policy. Expiry does not convert unknown into false; other known rules and the explicitly configured default-selection policy still apply.
- Freshness is receipt-based in the MVP. A valid retained message can initialize/refresh a fact, but its receipt does not prove that the upstream measurement is recent. Source timestamps/history are not implemented. Document this limitation and use a suitable upstream reporting cadence; an unchanged parked car is not automatically stale when expiry is disabled.

For split GPS, `max_coordinate_skew_seconds` defaults to 10 and must be finite and positive. Treat a pair as a small assembly operation:

1. After startup or the previous committed pair, require a newly received valid value for **both** components before committing another pair. Equal-valued updates count, so split publishers must publish both coordinates for each fix, even when only one changed.
2. Their valid receipt times must differ by no more than the configured skew, and both must satisfy the location input's optional freshness timeout. Coalesce intervening updates to the latest value of each component; do not combine a fresh axis with an arbitrarily old axis.
3. While an incomplete pair is assembling, retain the previous committed location only while it remains fresh. Set a non-extending assembly deadline to the first pending component's receipt time plus `max_coordinate_skew_seconds`. If the pair is still incomplete/incoherent at that deadline, expose location as unknown (`incoherent`) until a valid new pair is assembled. Repeated messages on just one axis cannot postpone that deadline.
4. A late second axis may pair with a sufficiently recent pending first axis; discard obsolete candidates. On a successful pair, atomically update the normalized location and its valid-receipt metadata. Its expiry deadline is based conservatively on the older component's receipt time.
5. JSON coordinates are atomic and do not use this pairing setting. Prefer JSON when the upstream cannot publish both split components consistently.

### Boolean-like inputs

Charging, plugged-in, charge completion, parked, locked, online, and source health are independently configured value mappings. Each has one topic, a list of values that mean true, and a list that mean false. Trim payload whitespace and compare ASCII text case-insensitively.

The same MQTT topic may feed more than one fact. For example, one charging-state topic could use these mappings:

```json5
charging: {
  topic: "vehicle/charging_state",
  true_values: ["Charging"],
  false_values: ["Stopped", "Complete", "NoPower", "Disconnected"],
},
plugged_in: {
  topic: "vehicle/charging_state",
  true_values: ["Charging", "Stopped", "Complete", "NoPower"],
  false_values: ["Disconnected"],
},
charge_complete: {
  topic: "vehicle/charging_state",
  true_values: ["Complete"],
  false_values: ["Charging", "Stopped", "NoPower", "Disconnected"],
},
```

This is only an example mapping; no Tesla-specific strings should be hard-coded in the rule engine. True and false value lists must be non-empty and must not overlap after normalization. An unrecognized value leaves the previous fact unchanged.

### Charging activity, connection, and completion

Keep these three facts independent:

- `charging` means the source reports active charging.
- `plugged_in` means the source reports a charger connection. It can stay true when charging stops.
- `charge_complete` means the source explicitly reports successful completion at its charge target. This optional input is unknown when omitted or not yet received.

When the BMS stops charging because the battery is full or the configured charge target is reached, the expected snapshot is `charging = false`, `plugged_in = true`, and `charge_complete = true` if completion is reported. Being plugged in with charging false can also mean waiting for a schedule, paused charging, or no power. Do not infer completion from inactivity or a fixed battery percentage, and do not infer a vehicle fault from inactivity or completion alone.

TeslaMate exposes both `plugged_in` and `charging_state`, with `Complete` among its charging-state values. A dedicated plugged-in topic can be configured instead of deriving connection from the shared status topic above. See the [TeslaMate MQTT documentation](https://docs.teslamate.org/docs/integrations/mqtt/). For any source, configure only mappings supported by that source's semantics.

When one MQTT message feeds several facts, decode and apply its valid updates together before evaluating outputs. For example, a shared `Complete` message updates charging, connection, and completion in one evaluation. Separate topics update independently using their last known values; missing connection or completion values must not be guessed from charging alone.

### Parked input

The optional `inputs.parked` uses the same boolean value mapping. Prefer an explicit parked/gear signal from the source. A boolean topic can map `true`/`false`; a gear topic can map `P` to true and `D`, `R`, and `N` to false when those meanings are supported by that source.

Do not infer parked from geofence membership, a locked vehicle, charging inactivity, or a lack of GPS updates. Omitted or unrecognized parked data remains unknown under the usual rules. A reminder requiring parked must not start until parked is explicitly true.

### Battery input

The battery topic contains a UTF-8 number from `0` through `100`. Store it as a percentage. Ignore invalid, non-finite, or out-of-range values without replacing the last valid value.

### Source health input

`inputs.source_healthy` optionally tracks the health reported by the upstream vehicle integration or logger. For example, TeslaMate documents `teslamate/cars/$car_id/healthy` as the health status of its logger for that vehicle. It does not establish that the car has no physical faults. See the [TeslaMate MQTT documentation](https://docs.teslamate.org/docs/integrations/mqtt/).

Use the same topic and true/false value mapping as the other boolean inputs. A TeslaMate configuration for a single car could be:

```json5
source_healthy: {
  topic: "teslamate/cars/1/healthy",
  true_values: ["true"],
  false_values: ["false"],
},
```

Other manufacturers or integrations can supply an equivalent topic through configuration. Do not require one or assume a standard topic name or payload across vendors.

- If omitted, do not subscribe or contribute a value to any fault aggregate. `source_healthy` remains unknown and its mapped outputs stay silent.
- If configured but no recognized value has arrived, it also remains unknown. Missing messages do not imply false.
- A recognized value updates only `source_healthy`. Empty, malformed, or unrecognized values follow the normal ignore-and-preserve behavior.
- Keep it independent of `online`, `vehicle_fault`, and `vehicle_fault_free`. A false value does not clear other facts, add a vehicle fault, or suppress unrelated outputs; a true value does not clear reported vehicle faults.
- This is the last reported source health. Publisher silence changes it only when this input explicitly configures a freshness timeout. This optional source-health fact is unrelated to Carstate's outgoing process heartbeat.

### Fault inputs

Faults are an optional list of named boolean-like inputs. Each entry uses the same configurable true/false value mapping as charging, lock, and online. This supports either one general fault-status topic or several specific warning topics.

```json5
faults: [
  {
    name: "reported_vehicle_fault",
    topic: "vehicle/fault",
    true_values: ["low tyre pressure", "low battery"],
    false_values: ["healthy"],
  },
  {
    name: "tpms_soft_warning_fl",
    topic: "vehicle/tpms_soft_warning_fl",
    true_values: ["true", "1", "warning"],
    false_values: ["false", "0", "normal"],
  },
]
```

Fault names must be unique, non-empty identifiers. No particular fault name, wheel layout, or TPMS topic is mandatory or hard-coded. An unconfigured fault is absent rather than false. An unrecognized value on a configured fault topic leaves that fault's previous value unchanged.

## Fixed application states

Application states are identified by a validated enum, not arbitrary strings inside the evaluator. Adding another state later should require a small, explicit Rust code change and tests. The MVP does not need a user-defined expression language.

Each state evaluates to `true`, `false`, or `unknown`:

| State name | Meaning |
| --- | --- |
| `location_within_outer` | At any configured location, distance is less than or equal to that location's outer radius. |
| `location_outer_band` | At any configured location, distance is greater than or equal to that location's inner radius and less than or equal to its outer radius. |
| `location_inner` | At any configured location, distance is strictly less than that location's inner radius. |
| `charging` | The source reports active charging. |
| `plugged_in` | The normalized plugged-in fact is true. |
| `charge_complete` | The source explicitly reports successful charge completion. |
| `parked` | The source explicitly reports that the car is parked. |
| `locked` | The normalized lock fact is true. |
| `battery_low` | Battery percentage is less than or equal to `battery_low_percent` in any `state_settings` entry that defines it. |
| `online` | The normalized online fact is true. |
| `source_healthy` | The upstream integration/logger reports itself healthy for this vehicle. |
| `vehicle_fault` | At least one configured fault is true, or an entry's battery-low condition is true and that same entry has `battery_low_is_fault: true`. |
| `vehicle_fault_free` | The known inverse of `vehicle_fault`, covering only the configured vehicle fault sources. |
| `location_inner_not_charging` | The car is inside an inner radius at any configured location and active charging is false, including completed or paused charging. |
| `location_inner_and_charging` | The car is inside an inner radius at any configured location and active charging is true. |
| `location_inner_and_plugged_in` | The car is inside an inner radius at any configured location and plugged in, regardless of charging activity or completion. |
| `location_inner_not_plugged_in` | The car is inside an inner radius at any configured location and plugged-in state is explicitly false. |
| `location_inner_parked_not_plugged_in` | The car is inside an inner radius at any configured location, parked is true, and plugged-in state is explicitly false. |
| `location_inner_plugged_in_not_charging` | The car is inside an inner radius at any configured location, plugged in, and active charging is false; the reason may be unknown. |
| `location_inner_charge_complete` | The car is inside an inner radius at any configured location, plugged in, active charging is false, and completion is explicitly true. |

### Multiple state-setting entries

`state_settings` is an array of named entries. Each entry may define target coordinates, inner/outer radii, a battery threshold, and whether its battery-low result counts as a fault. These settings all belong inside the array entry. Evaluate each configurable state's complete condition separately for each applicable entry, then combine those results with three-valued OR:

- Any true result makes the combined state true.
- All results false makes it false.
- No true result and at least one unknown result makes it unknown.
- An entry that omits a state's settings is excluded from that state's OR. It contributes neither false nor unknown.
- With no applicable entries, the combined state is unknown. An omitted or empty `state_settings` array therefore leaves location-based states and `battery_low` unknown; direct vehicle facts and separately configured named faults still operate.

For example, `location_inner` is true when the car is inside the inner radius of either home. `location_inner_not_plugged_in` is true when the car is inside either home's inner radius and explicitly unplugged. Array order has no priority, and distance comparisons always use the coordinates and radii from the same entry.

Overlaps are allowed. If the car is in home A's inner radius and home B's outer band, both `location_inner` and `location_outer_band` are true. Calculate each outer-band match within its own entry; do not derive the combined band by subtracting the combined inner state from the combined outer state. Output priorities arbitrate competing rules for a relay; they do not change these logical state results.

Evaluate all applicable entries before changing combined states or publishing outputs. A non-matching entry must not overwrite a match from another entry. Moving from a match at one home to a match at another without an observed change in the combined boolean must not itself republish the output, restart a timer episode, or advance the combined state's `last_changed_at` timestamp. Already scheduled blink phases continue normally.

Each entry's `battery_low_percent` compares against the same car's battery reading. Battery predicates are independent of GPS; a geofence in the same entry does not implicitly gate them. Combine the per-entry battery-low results with OR for `battery_low`. Entries without a battery threshold do not participate. Direct states such as `charging`, `plugged_in`, and `source_healthy` still reflect the single car's shared facts.

`battery_low_is_fault` defaults to false in each entry. Include that entry's battery-low result in `vehicle_fault` only when its flag is true and both its threshold and the battery input are configured. Do not OR all battery thresholds first and then apply another entry's fault flag. For example, with thresholds of 20% (fault enabled) and 30% (fault disabled), a battery reading of 25% makes `battery_low` true but contributes no true battery fault. Omitted battery input contributes no fault source; a configured battery input awaiting its first valid reading contributes unknown for enabled battery-fault entries.

With at least one configured location, the three base geofence states are unknown until valid GPS data exists. Composite location states use the same three-valued AND rules below for each entry. Raw fact states are unknown until their fact exists. Battery state is unknown until both a battery value and its threshold exist.

For composite states, use normal three-valued behavior. For example:

- Inner location `true` plus charging `false` makes `location_inner_not_charging` true.
- Inner location `true` plus charging `unknown` makes it unknown.
- Inner location `false` makes it false even if charging is unknown.

`location_inner_not_charging` is an activity indicator. Use `location_inner_not_plugged_in` for a plug-in reminder and `location_inner_and_plugged_in` for an indicator that should stay on after charging completes. When inside with connection unknown, both connection-based states are unknown; charging false alone must never trigger a plug-in reminder.

For a car inside the inner radius, these snapshots illustrate the distinction:

| Source condition | `charging` | `plugged_in` | `charge_complete` | `location_inner_not_charging` | `location_inner_not_plugged_in` |
| --- | --- | --- | --- | --- | --- |
| Charging actively | true | true | false | false | false |
| Charge target reached, still connected | false | true | true | true | false |
| Connected, paused or waiting | false | true | false | true | false |
| Connected, completion unavailable | false | true | unknown | true | false |
| Disconnected | false | false | false | true | true |
| Inactive, connection unavailable | false | unknown | unknown | true | unknown |

The example values require recognized upstream reports. Omitted completion input does not affect activity or connection-based states. `location_inner_charge_complete` follows three-valued AND across its four conditions, so an unknown completion value leaves it unknown when the other conditions hold.

`vehicle_fault` aggregates only configured fault sources; `source_healthy` is excluded. It is true as soon as any known fault source is true. It is false only when every configured fault source is known and false. Otherwise it is unknown. If no fault sources are configured, it remains unknown. `vehicle_fault_free` is the inverse of a known `vehicle_fault` and is also unknown when `vehicle_fault` is unknown. Use these explicit state names instead of the ambiguous state name `healthy`.

Calculate great-circle distance in meters using the Haversine formula. No geospatial dependency is required.

An output rule may reference any fixed application state. Only configured outputs are published. The orange reminder uses `location_inner_parked_not_plugged_in`; it remains false when the car is still plugged in after charging completes.

## Tasmota outputs, priorities, and built-in behaviors

Each output represents one physical relay/topic. It has a unique `name`, optional descriptive `color`, unique MQTT `topic`, `true_payload`/`false_payload`, `default_value` (false by default), and a `rules` array. Payloads are explicit target commands such as `ON` and `OFF`; never use MQTT `TOGGLE` for timer phases because retries could invert the relay unexpectedly.

Each rule has a name unique within its output, a fixed application `state`, optional boolean `when` (default true), an integer `priority`, and a `behavior`. A rule matches only when its admitted `control_states` value is known and equals `when`. Highest numeric priority wins within an output. Require distinct priorities within that output to avoid implicit list-order tie breaking. Different outputs are independent; to make a fault suppress other colors, configure a high-priority steady-off fault rule on those outputs.

Only the selected rule controls a relay. If no rule is eligible and at least one configured rule's admitted control state is known, use `default_value`. If all rule control states are unknown, the desired output is unknown and nothing is published. A simple output with a single `state` instead of `rules` is shorthand for one steady-true rule, with the normal default false output; reject configs that supply both forms.

The MVP has these fixed behavior macros, implemented as a Rust enum:

| Macro | Parameters | Behavior |
| --- | --- | --- |
| `steady` | `value: true/false` | Hold the chosen value while selected. |
| `blink_for` | `interval_seconds: X`, `duration_seconds: Y` | Blink for a bounded episode, then become ineligible so the next matching rule or default takes over. |

For `blink_for`, X is a complete ON/OFF cycle: ON for X/2, OFF for X/2, starting ON. X must be finite and at least twice the configured relay hold time, so the minimum default cycle is 2 seconds with one-second phases. Y is the elapsed-time limit and must be finite and at least X. This definition makes “once every X seconds” unambiguous. Timing is best-effort MQTT scheduling, not a hard real-time hardware guarantee. Additional built-in presets can be added in Rust later; no user scripts or arbitrary expression language are needed.

### Stack-light example

The physical stack is red, orange, green, blue from top to bottom. The example configuration maps these to `POWER1` through `POWER4` respectively; actual relay wiring is configurable and must not be hard-coded in the engine.

| Color / example relay | Example rules |
| --- | --- |
| Red / POWER1 | Steady on for `vehicle_fault`. |
| Orange / POWER2 | Flash for a bounded home/parked/unplugged reminder, then return to steady orange for that same condition. A higher-priority fault rule holds it off. |
| Green / POWER3 | Steady on while inside an inner geofence and plugged in, including after charge completion. A higher-priority fault rule holds it off. |
| Blue / POWER4 | Steady on in any configured outer band. A higher-priority fault rule holds it off. |

These color meanings are example mappings. The required orange behavior is expressed by two rules sharing `location_inner_parked_not_plugged_in`: a higher-priority `blink_for` reminder and a lower-priority `steady` baseline. The full example below uses X = 4 seconds and Y = 60 seconds. After 60 seconds, the reminder is exhausted and steady orange remains on, with no further relay cycling while the car stays parked and unplugged. The fallback can instead be configured off by changing the baseline rule's value.

### Jitter filtering of control states

Configure `jitter.max_changes` (positive integer, default **3**) and `jitter.cooldown_seconds` (finite number at least 1, default **10**). This is a **burst followed by a full cooldown**, not a rolling-window rate limit or a requirement that every GPS sample stay unchanged for 10 seconds.

Keep instantaneous `states` and separate admitted `control_states`. Apply one shared filter per fixed state **after** the per-entry OR, before any output rule uses that state; all rules referencing that state share its admitted value. Per-entry and raw combined diagnostics continue updating immediately. Filters are independent and do not rewrite raw facts or claim that delayed control states are a new consistent physical observation.

- Establish the first known control value immediately as a baseline without consuming a change. Subsequent admitted known true/false transitions consume the burst budget; duplicate values and a home-to-home handoff leaving the combined boolean unchanged consume nothing.
- After the Nth admitted transition, block further known transitions for a full `cooldown_seconds` measured from that Nth transition. With defaults, changes at t=0, 1, and 2 (after an earlier baseline) are admitted; the fourth cannot be admitted before t=12.
- Keep only the newest pending raw value during cooldown. Repeated or alternating messages cannot extend the deadline. At expiry, reevaluate and admit the latest different known value once, counting it as the first change in a new burst. If the latest value equals the admitted value, emit nothing and reset the budget.
- Before the limit is reached, a quiet period of at least `cooldown_seconds` since the last admitted transition resets the burst count. Thus isolated changes hours apart do not accumulate into a burst.
- Propagate unknown immediately to stop using an unavailable trigger; unknown neither consumes/resets the burst budget nor rearms a reminder. Preserve the last admitted known value internally. Recovery to that same known value is not another boolean transition; recovery to a different value follows the remaining budget/cooldown.
- Timer cancellation/rearming and rule selection use admitted control states, not suppressed raw transitions. A brief false excursion that was never admitted cannot rearm an exhausted reminder. Unknown suspends rather than rearms it.
- Use monotonic scheduled deadlines; a pending value must be reconsidered even if no new MQTT message arrives. This filtering also applies to noisy charging/plugged/parked/fault transitions, not only GPS. The first known fault is not delayed, but repeated fault changes are subject to the configured budget.
- **Do not apply this burst budget to programmed blink phases.** A four-second blink cycle is intentional output timing, not GPS jitter. Those phases use the separate relay hold limiter.

### Relay hold and command scheduling

`output_settings.min_hold_seconds` defaults to **1 second** and must be finite and at least 1; no config, priority, or macro may lower that floor. This is per relay, not a global delay between different relays.

- Every application-originated relay command goes through one serialized per-output scheduler. Space successful submissions to the same relay by at least its configured hold time, including steady changes, blink phases, preemption, defaults, retries, and reconnect/heartbeat/device resynchronization.
- Record the monotonic time of each accepted relay-command submission, including same-value resynchronization. Do not reset the hold clock for a definite pre-acceptance failure, a raw input, or an HTTP read; ambiguous attempts use the conservative retry settling policy below. The first command after startup/reconnect/device-online recovery waits at least one configured hold interval from that recovery event; this is a conservative settling delay, not proof of the physical relay's previous state.
- If a new decision arrives before the hold ends, replace the one pending command with the **current** desired value. At eligibility, reevaluate the winning rule, episode expiry, phase, and connection/device status. Drop obsolete pending values; do not play a queue of intermediate ON/OFF requests.
- A high-priority fault changes the selected rule immediately after its control state is admitted, but physical command submission still waits for the hold. Expiry and cancellation likewise cannot bypass the limiter.
- If a delayed blink phase is no longer current, skip it. Never compress missed phases to catch up, and never extend an episode's Y deadline to compensate for scheduling/hold delays.
- With 50% duty cycle, validate `blink_for.interval_seconds >= 2 * output_settings.min_hold_seconds`. Thus the minimum full cycle is now **2 seconds**, not the earlier one-second cycle. Each ON and OFF phase holds for at least one second at the default.
- Carstate enforces **its own submissions**, not end-to-end physical timing: network buffering/retransmission, Tasmota-local watchdog actions, other clients, and power cycling are outside that guarantee. Document matching device-side hold protection if an absolute physical one-second minimum is required. No app path may intentionally issue sub-second relay commands.

### Timer episodes and rearming

- Start an episode when a matching, armed `blink_for` rule wins at the first command-decision time when MQTT/device availability and the relay hold permit an attempt. Start ON and anchor Y at that eligibility time, not earlier during initial settling and not later upon successful retry. It starts once even if the input was already true in a retained startup message. After it starts, retries, later holds, outages, and preemption cannot extend Y. At the minimum X=2/Y=2/hold=1, startup settling is followed by one complete scheduled ON/OFF cycle, rather than consuming its ON phase.
- Measure Y with a monotonic clock. Duplicate input, HTTP reads, changes at another home that leave the combined trigger unchanged, logging delays, and MQTT reconnects must not reset or extend the deadline.
- At expiry, mark that rule exhausted for the current episode before selecting the next rule. An unchanged true condition must never start another episode every Y seconds.
- Rearm only after a recognized non-matching trigger value is admitted by the jitter filter, followed by an admitted matching value. For the reminder, explicitly plugging in, leaving the inner geofence, or no longer being parked clears the condition. Unknown values do not rearm it.
- If the trigger stops matching, cancel that rule's scheduled phases and reevaluate the output immediately. If it becomes unknown, suspend the behavior without rearming or guessing a false trigger.
- A higher-priority admitted rule preempts the blinker logically immediately; command submission still observes the relay hold. Its existing deadline continues while preempted; resuming before expiry uses only the remaining time, and resuming after expiry selects the fallback. A lower-priority reminder that has never won has not started an episode yet.
- Broker outages do not pause a started episode's deadline. Do not queue or replay a backlog of missed blink phases; after reconnecting, publish only the value required now (or the fallback if expired), and reuse the same client ID.
- One scheduler owns the desired value for each output. Use Tokio timers with cancellable/generation-checked deadlines so stale callbacks cannot override a new winner. Check expiry/preemption before processing a due phase.
- On a delayed wakeup, evaluate the current phase and deadline once and skip missed edges; do not execute a catch-up burst. Do not delay MQTT polling or timer scheduling while waiting for remote log sinks.
- Cancel timers during shutdown and do not generate new relay/heartbeat commands. The existing no-persistence scope remains: a process restart can start one fresh bounded episode after valid input; no cross-restart timer or jitter history is required. Tasmota's locally configured watchdog owns shutdown/crash fallback.

### Publishing rules

- Use MQTT QoS 1 by default and allow QoS 0 or 1 in configuration. Do not retain commands by default; make retention configurable.
- Publish the first known desired relay value, then only actual changes to that value. For a steady macro this follows rule decisions; for a blinker it also follows scheduled phases even when the logical car state is unchanged.
- Track desired value, selected rule, timer episode, and last submitted value separately for each output. A new winning rule that requests the existing relay value does not require another command.
- On expiry, switch directly to the fallback's desired value. Do not send an unnecessary OFF followed by ON when the fallback is steady ON.
- Reconnect, device-online, and enabled heartbeat resynchronization may republish an unchanged desired value through the hold limiter. Coalesce simultaneous recovery requests into one pass. They must not restart an episode, reset the jitter budget, or replay scheduled phases. Coalesce not-yet-submitted application commands to the latest desired value and never prequeue future blink phases. Already in-flight QoS 1 commands may be retransmitted by MQTT; use explicit idempotent payloads and resynchronize the current desired value without claiming exactly-once physical relay delivery.
- Preserve last valid vehicle facts on malformed input and transient errors. Blinking never modifies the logical `states` values or their last-change timestamps.
- Multiple rules may share one output; different output definitions must still have unique topics. Exactly one selected behavior controls each physical topic at a time.

## Publish retries, device availability, and control health

`mqtt_settings.publish_retry_initial_seconds` defaults to 1, `publish_retry_max_seconds` to 30, and `publish_timeout_seconds` to 5. Use finite positive values, with maximum backoff at least initial backoff.

- On a failed submission, retain one replaceable pending desired value per output and retry with bounded exponential backoff, even if no new MQTT input arrives. Reevaluate before every attempt; an old ON must not be retried after OFF/unknown, preemption, or expiry makes it obsolete.
- A successful client acceptance ends that application's submission retry, updates `last_submitted_*`, and resets its retry backoff. It does not prove broker acknowledgement or physical delivery.
- Bound a submission attempt by `publish_timeout_seconds`; a full/stalled client queue cannot block the evaluator or timer scheduler indefinitely. Definite pre-acceptance errors do not advance successful-submission timestamps. For ambiguous acceptance/timeouts, conservatively enforce a hold interval from the attempt before retrying, without falsely reporting successful submission.
- Use a clean MQTT session with no persistent backlog of application commands. The client may retransmit already in-flight QoS 1 work; explicit target payloads, current-value resynchronization, and device-side timing protection are still required.
- Fence queued work by connection generation. On disconnect/recovery, discard unsent relay commands and heartbeat pulses from the old generation before admitting a new session's work. A clean broker session alone does not clear a library's local send buffer; recreate the client/event loop with the same effective ID if necessary to discard obsolete buffered requests. Work already written to the network cannot be recalled and must not be described as an exactly-once or physical-timing guarantee.
- MQTT reconnect backoff and per-output submission retries are separate. Pause submissions while the transport/device is unavailable, retain only the latest decision, and schedule recovery without replaying missed phases.
- An optional `output_devices` array supplies named availability mappings (`topic`, `true_values`, `false_values`); an output's optional `device` references one entry. The example has one stack controller shared by all four relays. Without a device mapping, availability is unobserved and does not gate that output.
- A configured device starts unknown; defer its commands until a recognized online value. Invalidate device availability to unknown on broker disconnect/new session, then require a recognized availability message from that new session (retained allowed); do not reuse a pre-disconnect Online value as proof of current availability. Offline/unknown availability does not clear car facts, started timer deadlines, or jitter budgets. On its first recognized online value and later unknown/offline-to-online transitions, resynchronize its currently known outputs once through the hold limiter. Duplicate online values do not trigger more passes; simultaneous broker/device/heartbeat requests coalesce.
- Include availability topics in deduplicated subscription/acknowledgement tracking. This is device availability only, not physical relay acknowledgement or automatic correction of arbitrary manual changes. Enabled periodic heartbeat resynchronization also restores current known outputs after a device watchdog fallback that produced no connection event.

Define one cached `control_pipeline_healthy` predicate used by readiness and heartbeat gating: MQTT connected, required subscriptions acknowledged, configured output devices online, required workers making progress, and no unresolved failed/stalled publish submission. Retry/resynchronization work **continues while unready** and can restore this predicate; do not make recovery depend on already being ready.

`runtime_settings.worker_stall_seconds` defaults to 30, finite and at least 1. Supervise evaluator, timer scheduler, publisher, and MQTT-worker progress with idle-capable local checks at least once per second; `JoinHandle` not having exited is not sufficient proof of progress. Bounded I/O waits and intentional backoff/hold deadlines must be represented as expected waits, not mistaken for a dead worker. A stalled required control worker suppresses heartbeats and fails readiness; log the failure and stop an unrecoverable task failure cleanly. A failed submission keeps the pipeline unhealthy until the current required command succeeds, or reevaluation cancels that requirement as no longer applicable. Preserve the error in diagnostics either way.

Unknown/omitted/stale vehicle facts, a sleeping car, an intentional steady output, a blink's OFF phase, and an ordinary hold wait are not health failures. Relay availability is deliberately distinct from the vehicle's `online` and `source_healthy`.

## MQTT heartbeat and Tasmota-owned fallback

An optional top-level `heartbeat` block is **disabled by default**. Enable it after the matching Tasmota watchdog is installed. Configure its exact `topic`, non-empty `payload`, `interval_seconds` (default **120**), and expected device `timeout_seconds` (default **360**).

- Publish fresh pulses at QoS 0 with `retain: false`, regardless of relay-command QoS/retention. Never queue/replay missed pulses, configure a heartbeat Last Will, or refresh the watchdog using retained heartbeat data.
- Use a monotonic periodic schedule, with a first pulse on healthy startup/recovery. Every due pulse requests a coalesced resynchronization of all currently known desired outputs through the normal retry/hold scheduler. Submit the heartbeat only after that pass's current relay commands have been accepted and the control pipeline is healthy. Skip unknown outputs; do not guess values to make a pass complete.
- Normal hold waits may delay a pulse; they never justify bypassing settling. If a pass fails or becomes unavailable, suppress that pulse and continue recovery work. After recovery, perform one current resynchronization/pulse and resume the period, not a burst of missed heartbeats. A pulse must be newly authorized by the working control pipeline, not emitted by an independent timer that can outlive a failed controller.
- A failed/timed-out heartbeat submission is recorded, fails readiness, and schedules a fresh recovery cycle using the configured bounded publish backoff. This recovery attempt may run while unready because of its own previous heartbeat error: all **other** prerequisites (workers, connection, subscriptions, devices, and current relay resynchronization) must pass. A successful newly authorized pulse clears that heartbeat error. Do not require its old error to clear before attempting recovery, and do not retry a cached pulse without fresh authorization. Before the first pulse there is no previous heartbeat failure to gate initial readiness.
- Apply the connection-generation discard rule to heartbeat requests still buffered by the client. Revalidate authorization just before dispatch; stale unsent pulses must not survive a reconnect or control-worker failure. Already network-dispatched pulses cannot be revoked; this is another reason the device timeout is best effort, not a safety guarantee.
- Stop pulses on shutdown, broker/device unavailability, failed/stalled control delivery, or loss of permission to be the sole active writer. Optional unavailable vehicle facts alone do not stop them: this is **Carstate control health**, not car health.
- `timeout_seconds` is the timeout the operator must install on the Tasmota device. Carstate validates/logs/displays it; it does not remotely set that timeout or install scripts. Require whole-second interval/timeout values, interval at least 1, timeout at least twice the interval, and timeout greater than interval + configured relay hold + publish timeout + worker-stall allowance. The supplied RuleTimer example additionally requires timeout no greater than 65535 seconds.
- No explicit all-off/fallback MQTT command is sent on graceful shutdown. Stop accepting new relay work and heartbeats, cancel unsent work, and let the powered device's local watchdog choose its configured fallback. Already accepted network work cannot be recalled. No healthy heartbeats means fallback only when a working device-side watchdog has actually been installed.

### Device setup contract and example

The configured heartbeat may use the normal `cmnd/<device>/Event` topic with payload `carstate_heartbeat=alive`. A local rule refreshes a countdown on that exact event; expiry chooses the operator's fallback. Arm the countdown before networking initializes, so boot without a broker is covered. Tasmota documents `System#Init` before Wi-Fi/MQTT and `Rules#Timer` expiry triggers in its [Rules documentation](https://tasmota.github.io/docs/Rules/).

Illustrative all-off fallback for an **unused** rule/timer slot 1 and the example 360-second timeout:

```text
Rule1 ON System#Init DO RuleTimer1 360 ENDON ON Event#carstate_heartbeat=alive DO RuleTimer1 360 ENDON ON Rules#Timer=1 DO Backlog Power1 OFF; Power2 OFF; Power3 OFF; Power4 OFF ENDON
Rule1 4
Rule1 1
RuleTimer1 360
```

The commands define/enable the rule, disable one-shot matching so repeated pulses refresh it, and arm it immediately during installation. Match every `360` to the configured timeout. These commands use the documented [Event, Rule, and RuleTimer interface](https://tasmota.github.io/docs/Commands/#rules). **Do not paste over an occupied rule/timer slot**: inspect and merge the device's existing configuration first. Carstate and its implementation tests must not install these commands on a real device automatically.

Fallback payloads/colors are chosen in the device rule; all-off is only an example. Network reconnects and unrelated MQTT traffic must not reset this watchdog. Verify fallback with Carstate stopped, broker unavailable, and device restarted, then verify current-value restoration on controller recovery.

Verify that the installed firmware supports Rules. A completely arbitrary heartbeat topic needs a device-side subscription or bridge; Tasmota's custom `Subscribe` feature requires appropriate firmware support. The normal command-topic Event approach avoids that extra subscription feature. See [Tasmota MQTT subscription requirements](https://tasmota.github.io/docs/MQTT/#subscribeunsubscribe).

Never automatically erase retained broker data or modify device rules. During operator setup, check for old retained heartbeat/relay commands that could interfere. Device-local fallback can race network commands, so add matching device-side hold protection if the physical settle interval must be absolute. The app cannot verify watchdog installation or guarantee fallback during device power loss, disabled scripts, or firmware failure; expose that limitation honestly.

## Single-controller deployment

Exactly **one active writer per physical output set** is an operational requirement. One car per process and unique/generated MQTT IDs do not enforce this: two independently healthy processes could issue conflicting commands and keep each other's watchdog alive.

For the MVP, prevent overlap through deployment procedure rather than implementing distributed leader election: one replica, no autoscaling/blue-green overlap, and stop the old process and wait for termination before starting its replacement. A Kubernetes deployment should use a non-overlapping replacement strategy; do not rely on `replicas: 1` alone or treat a terminating/partitioned writer as proven stopped. The implementation handoff must document the equivalent procedure for a standalone service.

Reserve configured output and heartbeat topics for that controller. Other automation/manual clients must not compete during normal operation; the explicitly configured local Tasmota watchdog is the intended fallback owner. Do not use a shared MQTT client ID as a lock. Log the single-writer assumption at startup; do not report a verified lock/ownership guarantee that the MVP does not implement. Dry-run uses a separate diagnostic connection and is never a writer.

## Validation-only and dry-run modes

Provide mutually exclusive `--validate-config` and `--dry-run` CLI modes. Normal execution remains the default.

- `--validate-config`: load the same config/secrets, expand environment references, apply compatibility overrides/defaults, and run all schema/cross-field validations, including logger/error-catalog validation. Emit sanitized results through the minimal custom console logger, exit 0 on valid config (warnings allowed), and non-zero on errors. Do not open MQTT/HTTP connections/listeners, initialize remote log transports, publish anything, write generated IDs/config back, or modify Tasmota. Report blank-ID generation as the normal startup policy; validation need not generate a runtime identity.
- Warn once per unknown key path in recognized configuration sections, including nested rules and `state_settings` entries. Unknown configuration keys remain ignored, but typos such as `battery_low_precent` must not disappear silently. Unknown MQTT topics and extra vehicle JSON fields remain harmless input data, not startup config errors.
- `--dry-run`: validate first, then subscribe and execute the normal parser, freshness deadlines, state filters, rules, macros, hold limiter, retry simulation, and optional HTTP diagnostics using a recording/no-op output sink. Log structured `would_publish` decisions and simulated heartbeat eligibility. Never publish relay commands, heartbeats, setup commands, or an MQTT Last Will; do not clear retained messages.
- Always use a fresh `carstate_dryrun_XXXXXXXX` MQTT ID in dry-run, even when normal config specifies a fixed ID. Explain this override in a custom-log warning and expose the effective ID/mode. It must not reuse the live controller's identity.
- Keep simulated command timestamps/counters separate from real `last_submitted_*` fields and metrics; real output/heartbeat submission counts stay zero. Simulated acceptances advance only a private simulated hold clock. The actual state response clearly reports `mode: "dry_run"`, `publishing_enabled: false`, and hypothetical decisions.
- Dry-run may expose read-only HTTP using normal flags/authentication. Its readiness describes its observation/simulation pipeline, never an active physical controller. It cannot keep a device watchdog alive or be substituted for the production writer.
- No runtime hot reload is required; config changes take effect on restart. CLI help must explain mode side effects and the production single-writer requirement.

## Proposed JSON5 configuration

Keep the existing `CARSTATE_CONFIG_PATH` override and JSON5 format. Replace the current `mqtt_topics: Option<Vec<String>>` scaffold with typed input and output configuration. Follow the sibling [configuration contract](../styleguide/configuration.md): parse config/secrets, recursively resolve whole-value environment references, apply the small set of compatibility overrides, then validate the final object. Preserve defined environment values as strings (including empty strings), use null for absent variables, and do not interpolate partial strings or infer numeric/boolean values. Field schemas and documented compatibility parsers determine which types are allowed.

```json5
{
  http: {
    use_http: true,
    interface: "0.0.0.0",
    port: 3000,
    state_endpoint_enabled: false,
  },

  mqtt_settings: {
    ip: "mqtt.example.local",
    port: 1883,
    use_tls: false,
    validate_certs: true,
    keep_alive_seconds: 30,
    qos: 1,
    retain_commands: false,
    publish_retry_initial_seconds: 1,
    publish_retry_max_seconds: 30,
    publish_timeout_seconds: 5,
  },

  jitter: {
    max_changes: 3,
    cooldown_seconds: 10,
  },
  output_settings: {
    min_hold_seconds: 1, // Hard minimum: never less than 1.
  },
  runtime_settings: {
    worker_stall_seconds: 30,
  },

  output_devices: [
    {
      name: "garage_light",
      availability: {
        topic: "tele/garage-light/LWT",
        true_values: ["Online"],
        false_values: ["Offline"],
      },
    },
  ],

  heartbeat: {
    enabled: false, // Enable only after installing the matching device watchdog.
    topic: "cmnd/garage-light/Event",
    payload: "carstate_heartbeat=alive",
    interval_seconds: 120,
    timeout_seconds: 360, // Expected device timeout; not remotely installed by Carstate.
  },

  inputs: {
    location: {
      mode: "json",
      topic: "vehicle/location",
      stale_after_seconds: null, // Optional per-input expiry; null/omitted disables it.
    },
    charging: {
      topic: "vehicle/charging_state",
      true_values: ["Charging"],
      false_values: ["Stopped", "Complete", "NoPower", "Disconnected"],
    },
    plugged_in: {
      topic: "vehicle/charging_state",
      true_values: ["Charging", "Stopped", "Complete", "NoPower"],
      false_values: ["Disconnected"],
    },
    charge_complete: {
      topic: "vehicle/charging_state",
      true_values: ["Complete"],
      false_values: ["Charging", "Stopped", "NoPower", "Disconnected"],
    },
    parked: {
      topic: "vehicle/parked",
      true_values: ["true", "parked"],
      false_values: ["false", "not_parked"],
    },
    locked: {
      topic: "vehicle/locked",
      true_values: ["true", "locked", "1"],
      false_values: ["false", "unlocked", "0"],
    },
    battery: {
      topic: "vehicle/battery_level",
    },
    online: {
      topic: "vehicle/online",
      true_values: ["true", "online", "1"],
      false_values: ["false", "offline", "0"],
    },
    source_healthy: {
      topic: "vehicle/healthy",
      true_values: ["true"],
      false_values: ["false"],
    },
    faults: [
      {
        name: "reported_vehicle_fault",
        topic: "vehicle/fault",
        true_values: ["low tyre pressure", "low battery"],
        false_values: ["healthy"],
      },
      {
        name: "tpms_soft_warning_fl",
        topic: "vehicle/tpms_soft_warning_fl",
        true_values: ["true", "1", "warning"],
        false_values: ["false", "0", "normal"],
      },
    ],
  },

  state_settings: [
    {
      name: "home",
      target_latitude: 49.2827,
      target_longitude: -123.1207,
      outer_radius_meters: 300.0,
      inner_radius_meters: 50.0,
      battery_low_percent: 20.0,
      battery_low_is_fault: true,
    },
    {
      name: "second_home",
      target_latitude: 49.8427,
      target_longitude: -123.4257,
      outer_radius_meters: 300.0,
      inner_radius_meters: 50.0,
      battery_low_percent: 30.0,
      battery_low_is_fault: false,
    },
  ],

  logging: {
    errorFile: "./config/errors.json5",
    sinks: {
      console: { enabled: true, format: "json", levels: ["info", "warn", "error"] },
    },
    gates: {
      SERVICE_BOOT_DIAGNOSTICS: { enabled: true, level: "info", console: true },
      OUTPUT_BLINK_PHASE: { enabled: false },
    },
    // Omit hard-coded Kubernetes/instance metadata so runtime values are used.
  },

  outputs: [
    {
      name: "red",
      color: "red",
      device: "garage_light",
      topic: "cmnd/garage-light/POWER1",
      true_payload: "ON",
      false_payload: "OFF",
      default_value: false,
      rules: [
        {
          name: "fault",
          state: "vehicle_fault",
          priority: 100,
          behavior: { macro: "steady", value: true },
        },
      ],
    },
    {
      name: "orange",
      color: "orange",
      device: "garage_light",
      topic: "cmnd/garage-light/POWER2",
      true_payload: "ON",
      false_payload: "OFF",
      default_value: false,
      rules: [
        {
          name: "fault_override",
          state: "vehicle_fault",
          priority: 100,
          behavior: { macro: "steady", value: false },
        },
        {
          name: "plug_in_reminder",
          state: "location_inner_parked_not_plugged_in",
          priority: 20,
          behavior: { macro: "blink_for", interval_seconds: 4.0, duration_seconds: 60.0 },
        },
        {
          name: "parked_unplugged",
          state: "location_inner_parked_not_plugged_in",
          priority: 10,
          behavior: { macro: "steady", value: true },
        },
      ],
    },
    {
      name: "green",
      color: "green",
      device: "garage_light",
      topic: "cmnd/garage-light/POWER3",
      true_payload: "ON",
      false_payload: "OFF",
      default_value: false,
      rules: [
        {
          name: "fault_override",
          state: "vehicle_fault",
          priority: 100,
          behavior: { macro: "steady", value: false },
        },
        {
          name: "home_connected",
          state: "location_inner_and_plugged_in",
          priority: 10,
          behavior: { macro: "steady", value: true },
        },
      ],
    },
    {
      name: "blue",
      color: "blue",
      device: "garage_light",
      topic: "cmnd/garage-light/POWER4",
      true_payload: "ON",
      false_payload: "OFF",
      default_value: false,
      rules: [
        {
          name: "fault_override",
          state: "vehicle_fault",
          priority: 100,
          behavior: { macro: "steady", value: false },
        },
        {
          name: "outer_band",
          state: "location_outer_band",
          priority: 10,
          behavior: { macro: "steady", value: true },
        },
      ],
    },
  ],
}
```

Split GPS mode replaces the JSON location input with:

```json5
location: {
  mode: "split",
  latitude_topic: "vehicle/latitude",
  longitude_topic: "vehicle/longitude",
  max_coordinate_skew_seconds: 10,
  stale_after_seconds: null,
},
```

Every input is optional. Omitted inputs and state settings leave dependent application states unknown, subject to the three-valued evaluation rules, and outputs mapped to unknown states remain silent. Omitting `state_settings` is equivalent to `[]`. An entry can omit its geofence, battery settings, or both; omitted settings do not participate in the relevant aggregate. These omissions are not startup errors, so a deployment can use only the supported vehicle-wide inputs. Log one startup warning for an output that cannot currently be produced from the configured inputs/settings.

Unknown vehicle payload fields are ignored. Unknown configuration extension keys are ignored **with a startup/validation warning naming their key path**, so optionality does not hide misspelled settings. Unknown state or behavior-macro names are configuration errors because they are likely spelling mistakes. The `logging` block follows the custom logger's own schema and validation. Absent new top-level blocks use their documented defaults: jitter 3 changes/10 seconds, relay hold 1 second, publish retry 1–30 seconds with a 5-second attempt timeout, worker stall 30 seconds, no output-device mappings, and heartbeat disabled.

Validation must also cover:

- Broker and HTTP ports
- `http.state_endpoint_enabled` must be a boolean when present and defaults to false
- After all defaults and environment overrides, `http.state_endpoint_enabled: true` requires `http.use_http: true`; otherwise fail startup with a configuration error before opening any network connection or listener
- An enabled state endpoint requires a non-blank `http_state_token` in secrets; the token is optional when the endpoint is disabled
- Non-empty exact MQTT topics for vehicle inputs, outputs, availability, and enabled heartbeat; reject wildcards and NUL characters in this MVP's topic fields. Reject heartbeat/relay publish-topic collisions and publish topics reused as vehicle/availability inputs
- `state_settings` must be an array when present; an empty array is valid
- Every supplied entry must have a unique, non-empty name; a geofence may be omitted, but if any of its fields are supplied the target coordinates and both radii must be complete
- Finite GPS coordinates in range and finite positive radii with `inner_radius_meters < outer_radius_meters`, checked within each entry
- Within each entry, `battery_low_percent` must be finite and from 0 through 100 when present, and `battery_low_is_fault` must be a boolean when present; a flag without a threshold supplies no battery-fault source
- QoS limited to 0 or 1
- Non-overlapping boolean true/false mappings
- Unique, non-empty configured fault names
- Known application state names
- Non-empty output payloads
- Unique output names/topics, non-empty rule lists, unique rule names and distinct integer priorities within each output, and no simultaneous `state` shorthand and `rules` array
- Known behavior macros with correctly typed parameters; finite `min_hold_seconds >= 1`, `interval_seconds >= 2 * min_hold_seconds`, and `duration_seconds >= interval_seconds`, using checked duration/deadline conversion
- Positive integer `jitter.max_changes` and finite `jitter.cooldown_seconds >= 1`; neither may be zero, negative, or silently coerced from an invalid value
- Null/omitted or finite positive per-input `stale_after_seconds`; finite positive `max_coordinate_skew_seconds` only in split GPS mode
- Finite positive publish retry/timeout settings, retry maximum at least initial delay, and finite `worker_stall_seconds >= 1`
- Unique non-empty output-device names, valid availability boolean mappings, and every supplied output `device` referencing a declared device; an omitted/empty device array is valid
- Boolean `heartbeat.enabled`; when enabled, exact non-empty topic/payload and valid whole-second interval/timeout values with the watchdog margins described above. Validate any supplied timing fields even when disabled; topic/payload may be omitted when disabled
- The validation-only and dry-run flags cannot be combined
- At least one input and one output

## Secrets and environment overrides

Broker credentials must not be committed. Continue the direction of the existing `Secrets` type by loading an ignored JSON5 file from `CARSTATE_SECRETS_PATH`, defaulting to `./config/secrets.json5`:

```json5
{
  mqtt_username: "carstate",
  mqtt_password: "replace-me",
  mqtt_client_id: "", // Generate an ID for this process and warn at startup.
  http_state_token: "", // Required only when the state endpoint is enabled.
}
```

Username and password may be empty for an anonymous broker. Errors and debug output must never contain the password.

If `mqtt_client_id` is omitted, empty, or whitespace-only, generate an ID once during startup in the form `carstate_XXXXXXXX`, where the suffix is eight random uppercase letters/digits from the operating system's random source. Treat missing/blank ID as a supported fallback, not a configuration error. Preserve an explicitly configured non-blank ID unchanged.

Emit one startup warning identifying the generated ID and explaining that setting `mqtt_client_id` provides a stable identity. Never include the username or password in this warning. Reuse the generated ID for every reconnect during this process lifetime; generate a fresh ID on the next process start and do not write it back to the secrets file. Do not use a constant fallback ID shared by all instances. If random ID generation fails, return a startup error.

`http_state_token` protects the administrator state endpoint with a bearer token. Never return or log it. The effective MQTT client ID is an operational identifier and is intentionally included in the authenticated `/state` response, even though it is loaded alongside credentials.

Keep these existing environment overrides:

- `CARSTATE_CONFIG_PATH`
- `CARSTATE_HTTP_PORT`
- `CARSTATE_USE_HTTP`
- `CARSTATE_HTTP_INT`

Config/secrets file values are loaded and environment references expanded first; explicit compatibility overrides are applied afterward. Validate afterward in every CLI mode. Do not add a separate environment-variable alias for every new jitter, heartbeat, or timer field. Dry-run's diagnostic MQTT ID is a documented mode-specific override; validation-only never starts a client.

## Required custom logging and startup diagnostics

Custom-logger integration is an MVP requirement. Follow the [styleguide logging contract](../styleguide/logging/logging.md), its [startup diagnostics contract](../styleguide/deployment.md#startup-logging), and the bundled [Rust logger documentation](./src/logger/README.md). Use the existing crate as a path dependency:

```toml
styleguide-logger = { path = "src/logger" }
```

Replace all application-owned `println!`, `eprintln!`, `dbg!`, ad hoc console output, and unformatted top-level error reporting with this logger. This includes config loading, generated-client-ID warnings, HTTP enabled/disabled and listener events, MQTT lifecycle, rejected input, state/rule changes, timers, publication errors, authentication errors, and shutdown. Logger internals and standalone logger test/CLI output are not application log call sites.

Initialize a minimal custom console logger before loading application config so config/secrets/logging failures can also be reported through the custom logger. Then construct one configured logger and share clones (or a thin application wrapper) across tasks. Use `generate_log`, `generate_error`, and `wrap_error`; register distinct logger/error keys per producing code path and create an application error catalog at `./config/errors.json5` using the existing Cargo helpers. Preserve HTTP correlation IDs in the logger's `correlation_id` argument. If dependency logging is enabled, bridge it into the custom logger instead of adding a competing console logger.

Logging calls are async. Keep them awaited or owned by a bounded, supervised logging worker; remote sinks must not block the MQTT poller or blink scheduler. Startup diagnostics must be attempted before normal work. Observe sink failures without recursively logging through the same failed sink; a separate minimal custom console logger can report logger transport failures. Drain accepted log work with a bounded shutdown wait. The existing logger has no background queue of its own, so the application must own any queue it introduces.

### Startup event

Emit `SERVICE_BOOT_DIAGNOSTICS` through the custom logger after final config, client ID, instance identity, and build metadata are resolved, before readiness or normal MQTT processing. Include:

- `service`, `version`, `buildHash`, and `buildNumber` when supplied by build metadata. Do not invent a build number; the required baseline is version plus commit hash.
- Process start time, PID, platform/architecture, hostname, and effective `instanceId`.
- Effective config/secrets file paths, MQTT client ID and whether it was generated, broker host/port and TLS flags, and counts of configured inputs, state-setting entries, and outputs.
- Explicit `httpEnabled` and `stateEndpointEnabled` booleans, configured HTTP interface/port, and `stateTokenConfigured`/`mqttCredentialsConfigured` booleans. Emit this event with `httpEnabled: false` too; do not place startup logging inside the HTTP-enabled branch.
- Enabled log sink names/formats and whether Kubernetes metadata is attached. Report jitter budget/cooldown, relay hold, enabled timer presets and their intervals/durations, input expiry/coherence policies, retry/stall limits, and heartbeat enabled/topic/interval/expected device timeout.
- Execution mode, whether output publication is enabled, and the operational single-writer requirement. Never imply that a watchdog installation or distributed lock was verified.

A later `HTTP_LISTENING` event confirms the actual bound listener; the boot event must not claim binding already succeeded. The generated-client-ID warning also uses the custom logger. Ensure the shipped sink levels/gates emit startup info and warnings; do not copy an example gate that suppresses the boot event. Use JSON console logs by default for container deployments, with only the configured sinks enabled.

Log behavior start, preemption, cancellation, expiry, fallback, and rearming as structured lifecycle events with output/rule names. Include jitter cooldown/admission, stale/coherence changes, failed/recovered submission, device availability, and heartbeat suppression/recovery. Successful periodic heartbeats and resynchronizations should use a gated debug event, not a repeating startup/info diagnostic. Gate noisy per-phase and duplicate-input logs at debug level; the example disables `OUTPUT_BLINK_PHASE` by default. Avoid turning every blink edge into an info log.

### Kubernetes and instance identity

Preserve the logger's support for `LOG_*` settings and these metadata variables:

- `K8S_POD_NAME`
- `K8S_DEPLOYMENT`
- `K8S_NAMESPACE`
- `K8S_POD_IP`
- `K8S_POD_IPS`
- `K8S_NODE_NAME`

When any recognized Kubernetes metadata variable is non-empty and no explicit metadata-enable choice was supplied, Carstate must enable the effective logger's Kubernetes attachment automatically. Honor deliberate `LOG_K8S_METADATA_ENABLED` or JSON5 `logging.kubernetes.enabled` choices using the logger's documented precedence. Do not copy the reusable example's hard-coded `enabled: false` into shipped app config: it would override runtime environment settings. Preserve populated metadata fields and omit unavailable ones. The native `kubernetes` event field should carry this metadata on every event, without duplicating it in `context`; if attachment is deliberately disabled, the startup event should still include available Kubernetes diagnostics once as described by the styleguide.

Use non-empty `INSTANCE_ID` as the application instance identity when set; otherwise use `K8S_POD_NAME`, then `HOSTNAME`, then the effective MQTT client ID. A small wrapper should attach the resolved `instanceId`, hostname when available, and PID consistently to application log contexts. Do not invent random per-event instance IDs. Preserve the logger's existing `LOG_SYSLOG_HOSTNAME`, `LOG_SYSLOG_APP_NAME`, and `LOG_SYSLOG_PROC_ID` handling when that sink is enabled.

Test instance/Kubernetes enrichment on ordinary events and errors, not only the boot log. Never dump the entire environment, resolved secrets, raw vehicle payloads, or credential-bearing config/error objects. Reuse the logger's JSON5 environment expansion and validation; application input optionality does not disable logging-config validation.

## Runtime structure

Suggested module boundaries:

- `config.rs`: JSON5/secrets loading, defaults, environment overrides, and validation.
- `model.rs`: normalized facts, input payload models, application state enum, three-valued result type, freshness/coherence metadata, raw/control states, timestamps, and runtime diagnostic snapshot types.
- `inputs.rs`: topic routing and conversion of MQTT payloads into fact updates.
- `rules.rs`: pure Haversine, per-entry location/battery evaluation, three-valued OR aggregation, and direct vehicle-state evaluation.
- `outputs.rs`: per-output priority selection, desired relay values, payload selection, hold limiter, coalescing retries/resynchronization, and recording/dry-run sink.
- `behaviors.rs` (or equivalent): built-in steady/blink macros, episode state, jitter filters, and monotonic deadlines.
- `heartbeat.rs` (or equivalent): health-gated pulse scheduling and current-output resynchronization, never independent false-health announcements.
- Application logging wrapper: shared custom logger, instance metadata, error handling, and any bounded logging dispatch.
- `mqtt.rs`: client setup, vehicle/availability topic routing, subscription/event loop, reconnect handling, and bounded publish submission.
- `http.rs`: styleguide-compatible liveness/readiness JSON, correlation IDs, and the optional authenticated JSON state endpoint.
- `main.rs`: startup, shared readiness/state snapshot, task supervision, and shutdown.

Keep parsing and rule evaluation independent of MQTT so they can be unit tested. MQTT and HTTP must run concurrently; awaiting the HTTP server before starting MQTT would block the core service.

Use the existing Tokio, `rumqttc`, Serde/JSON5, and Axum dependencies plus the bundled `styleguide-logger` path dependency. The timer scheduler, publisher, heartbeat controller, and logging worker (if used) are supervised alongside MQTT and HTTP. Keep evaluation, filtering, priority selection, behavior timing, and retries testable with injected clocks and a recording MQTT sink. Implement CLI modes before enabling physical command output.

## HTTP health checks and build metadata

Follow the [deployment and probe contract in the sibling styleguide](../styleguide/deployment.md#probe-endpoints). Expose exactly `/livez` and `/readyz` as probes when HTTP is enabled. Both return `application/json` with `ok`, `probe`, `service`, `version`, `buildHash`, and `correlation_id`. These probes remain unauthenticated; detailed diagnostics belong in the authenticated `/state` endpoint.

- `GET /livez` returns HTTP 200 with `ok: true` when the process and HTTP listener can answer. It must be cheap and dependency-free: no broker, DNS, filesystem, or other external calls in its handler.
- `GET /readyz` returns HTTP 200 with `ok: true` only when the cached control-pipeline predicate is healthy: workers progressing, MQTT connected, all unique vehicle/availability subscriptions acknowledged, configured output devices online, and no unresolved failed/stalled submission. A TCP connection or a not-yet-exited worker handle alone is insufficient. Dry-run evaluates the equivalent observation/simulation checks without claiming a publishing controller.
- Before readiness, during a broker outage, after a failed subscription, or while shutting down, `/readyz` returns HTTP 500 with `ok: false`. Use 500 as specified by the styleguide, not 503. Liveness remains 200 during an external outage.
- Readiness includes a `checks` object for required MQTT/worker/control-delivery checks. Each entry contains `ok`, a short generic `description`, and `latency_ms` measuring the bounded local check. Do not open an extra broker connection or publish test messages on each probe.

Example `/livez` response:

```json
{
  "ok": true,
  "probe": "liveness",
  "service": "carstate",
  "version": "0.1.0",
  "buildHash": "unknown",
  "correlation_id": "0198f3f5-d9af-7a5b-8f1c-fcb2d40c8241"
}
```

Example `/readyz` response during a broker outage (HTTP 500):

```json
{
  "ok": false,
  "probe": "readiness",
  "service": "carstate",
  "version": "0.1.0",
  "buildHash": "unknown",
  "correlation_id": "0198f3f5-d9af-7a5b-8f1c-fcb2d40c8242",
  "checks": {
    "mqtt": { "ok": false, "description": "MQTT session or subscriptions unavailable", "latency_ms": 0.1 },
    "workers": { "ok": true, "description": "Required workers progressing", "latency_ms": 0.1 },
    "control": { "ok": false, "description": "Control delivery unavailable", "latency_ms": 0.1 }
  }
}
```

Give each HTTP call one correlation ID. Accept `x-correlation-id` when it is a valid canonical UUID; generate a UUID when absent or malformed. Echo the same value in the `x-correlation-id` response header and the JSON `correlation_id` field, including `/state` responses and application-owned errors. Do not add a second request-ID alias.

Load or embed build metadata once at startup and reuse it across startup logs, both probes, and `/state`. `version` comes from the Cargo package version; `buildHash` comes from build-time Git metadata. A local/non-release build may fall back to supplied `BUILD_HASH` or `unknown`; a release build must supply the real commit hash. Do not run Git or reread metadata files in handlers. Emit the custom logger's startup diagnostics event containing `version` and `buildHash` before reporting readiness or processing normal MQTT work. Container image publishing remains a separate task; these metadata requirements apply to the service now.

Do not include broker addresses, client IDs, topics, configuration revisions, counts, entry names, full errors, or credentials in probe responses. The listener must use the processed `app_config.http.interface` and `app_config.http.port`; it must not reread a hard-coded port.

These probes describe Carstate itself. An absent, unknown, or false `source_healthy` value does not change liveness/readiness. Optional vehicle facts are not required dependencies; readiness does not wait for the car to be online or for every input to publish a value. Configured output-device availability is a separate control dependency. Heartbeat emission uses the same predicate but is not itself a prerequisite for the first ready transition; retries and resynchronization continue while readiness is false.

## Optional HTTP state endpoint

Add `GET /state` on the existing HTTP listener. Enable it with `http.state_endpoint_enabled: true`; the default is false, including when the setting is omitted. When disabled and HTTP is enabled, the route is not registered and requests receive HTTP 404. Setting `http.use_http: false` together with `http.state_endpoint_enabled: true` is a fatal configuration error, including when produced by an environment override. Return a non-zero exit with a message such as `http.state_endpoint_enabled requires http.use_http = true` before starting MQTT or HTTP. With both settings false, no HTTP listener starts. Enabling the state endpoint does not otherwise change the health-check routes.

As required by the styleguide for detailed runtime state, authenticate this endpoint with `Authorization: Bearer <http_state_token>`. When enabled, reject missing/invalid credentials with HTTP 401, `WWW-Authenticate: Bearer`, and a generic JSON error with correlation metadata; do not return any snapshot data. Validate that the token is configured before startup and compare credentials without leaking the token through responses or logs. Disabling the route still returns 404, not 401.

For an authenticated request, return HTTP 200 with `Content-Type: application/json` and `Cache-Control: no-store`. Apply `no-store` to authentication errors too. The response contains:

- `service`, `version`, `buildHash`, and `correlation_id`: the same identity/build/correlation fields used by the probes.
- `generated_at`: the UTC time the snapshot was taken.
- `runtime`: process `started_at`, monotonic `uptime_seconds`, execution `mode` (`normal` or `dry_run`), `publishing_enabled`, current `ready`/`control_pipeline_healthy` booleans, required-worker progress/status metadata, and effective HTTP interface, port, and feature flags. Report single-writer enforcement as an operational assumption, not a verified lock.
- `mqtt`: the effective `client_id`, a `client_id_generated` boolean, broker host/port and TLS settings, a tracked `connected` boolean, configured QoS/command retention, and `credentials_configured` as a boolean only. Include every unique vehicle-input/availability topic with its roles, requested QoS, and tracked subscription `active` value, plus `last_received_at`, `last_submitted_at`, `last_error`, and process-lifetime counters for received messages, rejected messages, publish submission failures, and successful reconnects. `last_error` is null or a short application-defined code and timestamp, never an unsanitized exception or connection string.
- `facts`: every normalized vehicle fact, including optional facts, with its effective `value`, `last_changed_at`, valid-update `last_received_at`, freshness deadline/age, unknown reason, and last-known diagnostics when unavailable. Include split-location component receipts/pending-pair/coherence metadata. Named faults appear under `facts.faults`, with one such entry per configured fault name; an omitted fault list is an empty object.
- `states`: every fixed application state, keyed by its enum/configuration name, with its `value` and `last_changed_at`, whether or not it has an output mapping. Configurable states contain the immediate combined OR results; output rules use their separately filtered control counterparts.
- `control_states`: each fixed state's admitted value, `last_changed_at`, last admitted known value/time, burst count/limit, cooldown deadline, and latest pending value. Raw `states` must not be hidden by jitter suppression.
- `devices`: configured output-device availability, receipt/change times, and resynchronization status, never an inferred physical relay position.
- `heartbeat`: enabled/suppressed status and reason, configured topic/interval/expected timeout, last attempt/submission and next due time, pending resynchronization status, and `watchdog_verification: "not_verified"`. Do not imply that setting a timeout in app config installed a device rule.
- `entry_states`: per-entry results keyed by each `state_settings` entry's name. Include the applicable `location_*` states and `battery_low`, each with its own `value` and `last_changed_at`. Omit states whose settings are absent in that entry; keep null values for applicable states awaiting data. This is an empty object when `state_settings` is empty.
- `outputs`: one entry per configured output with its name/color, topic, effective QoS/retention, `selected_rule` (null for default/unknown), `active_macro`, desired relay boolean and `last_changed_at`, selected `desired_payload`, `pending_submission`, `last_submitted_payload`, and `last_submitted_at`. Include per-rule trigger value, priority, macro status, episode start/deadline, next scheduled transition, remaining duration, and exhausted/rearm status where applicable. Unknown values/payloads and inactive timer timestamps are null. Expose UTC projections of deadlines for diagnostics while scheduling internally with a monotonic clock. Also include next eligible submission/hold remaining, latest pending command/reason, retry count/deadline and sanitized error status, and separate dry-run would-publish records where applicable. Submission means accepted by the MQTT client; it does not claim broker acknowledgement or physical relay state.

Serialize boolean values as JSON `true`/`false`, battery level as a number, and location as a latitude/longitude object. Serialize unknown values as JSON `null`. Use explicit response types for the allowlisted runtime details rather than serializing configuration/secrets objects. Never return usernames, passwords, HTTP bearer tokens, TLS keys, raw configuration, or raw incoming MQTT payloads. The endpoint is read-only and must not trigger MQTT commands.

Example response excerpt (the actual response includes all runtime fields, subscriptions, facts, raw/control states, devices, heartbeat, applicable per-entry states, and outputs):

```json
{
  "service": "carstate",
  "version": "0.1.0",
  "buildHash": "unknown",
  "correlation_id": "0198f3f5-d9af-7a5b-8f1c-fcb2d40c8243",
  "generated_at": "2026-09-07T12:05:00.000Z",
  "runtime": {
    "started_at": "2026-09-07T10:00:00.000Z",
    "uptime_seconds": 7500,
    "mode": "normal",
    "publishing_enabled": true,
    "ready": true
  },
  "mqtt": {
    "client_id": "carstate_7K4N9Q2X",
    "client_id_generated": true,
    "broker": { "host": "mqtt.example.local", "port": 1883, "use_tls": false, "validate_certs": true },
    "connected": { "value": true, "last_changed_at": "2026-09-07T10:00:00.000Z" },
    "credentials_configured": true,
    "subscriptions": [
      {
        "topic": "vehicle/charging_state",
        "qos": 1,
        "active": { "value": true, "last_changed_at": "2026-09-07T10:00:00.000Z" }
      }
    ],
    "last_received_at": "2026-09-07T12:00:00.000Z",
    "last_submitted_at": "2026-09-07T12:00:00.000Z",
    "last_error": null
  },
  "facts": {
    "charging": { "value": false, "last_changed_at": "2026-09-07T12:00:00.000Z" },
    "plugged_in": { "value": true, "last_changed_at": "2026-09-07T10:00:00.000Z" },
    "charge_complete": { "value": true, "last_changed_at": "2026-09-07T12:00:00.000Z" },
    "source_healthy": { "value": null, "last_changed_at": null },
    "faults": {}
  },
  "states": {
    "location_inner_not_charging": { "value": true, "last_changed_at": "2026-09-07T12:00:00.000Z" },
    "location_inner_and_charging": { "value": false, "last_changed_at": "2026-09-07T12:00:00.000Z" },
    "location_inner_not_plugged_in": { "value": false, "last_changed_at": "2026-09-07T10:00:00.000Z" },
    "location_inner_charge_complete": { "value": true, "last_changed_at": "2026-09-07T12:00:00.000Z" }
  },
  "entry_states": {
    "home": {
      "location_inner": { "value": true, "last_changed_at": "2026-09-07T10:00:00.000Z" },
      "battery_low": { "value": false, "last_changed_at": "2026-09-07T10:00:00.000Z" }
    },
    "second_home": {
      "location_inner": { "value": false, "last_changed_at": "2026-09-07T10:00:00.000Z" },
      "battery_low": { "value": false, "last_changed_at": "2026-09-07T10:00:00.000Z" }
    }
  },
  "outputs": [
    {
      "name": "orange",
      "color": "orange",
      "topic": "cmnd/garage-light/POWER2",
      "selected_rule": null,
      "active_macro": "steady",
      "qos": 1,
      "retain": false,
      "desired": { "value": false, "last_changed_at": "2026-09-07T10:00:00.000Z" },
      "desired_payload": "OFF",
      "pending_submission": false,
      "last_submitted_payload": "OFF",
      "last_submitted_at": "2026-09-07T10:00:00.000Z"
    }
  ]
}
```

Track timestamp metadata alongside the values shown in the earlier `VehicleState` sketch:

- Format timestamps as UTC RFC 3339 strings with millisecond precision. Use the time Carstate applies an update, not an assumed upstream event time.
- At startup, unknown values have `last_changed_at: null`. The first transition from unknown to a known value sets the timestamp, even when that value is false.
- Advance a fact's value-change timestamp only when its effective normalized value changes, including becoming unknown on expiry/coherence loss and recovery to known. Valid duplicates advance its separate valid-receipt time and expiry deadline, not its value-change timestamp; rejected input advances neither.
- Advance a derived state's timestamp only when its evaluated value changes. A dependency changing without changing the result does not advance the state's timestamp. Blink phases update the output's desired-value timestamp, never the logical trigger state's timestamp; timer/selected-rule metadata is tracked separately.
- Track per-entry timestamps independently from combined timestamps. A change in which entry matches updates the affected `entry_states` values but does not change a combined state's timestamp if its boolean is unchanged.
- Apply one MQTT message's fact updates, raw-state evaluation, filter/decision evaluation, and timestamps as a single snapshot update. Do the same for scheduled expiry/cooldown/phase events. Readers must not see facts from that update paired with stale raw states; a delayed control value must be explicitly represented as filtered/pending, not a torn snapshot.
- Reading the endpoint or republishing unchanged outputs after reconnecting does not change these timestamps. They reset on process restart; no persistence is required.

Operational counters and last-received/submitted times track transport activity separately from value-change timestamps. A duplicate MQTT message can advance `last_received_at` and the receive counter without changing any fact/state timestamp. On disconnect, mark connection and subscriptions inactive without clearing the client ID, vehicle facts, or last submission records. A later successful subscription marks that topic active again. Reconnect resubmission updates submission times but not unchanged desired-state timestamps.

Serve the current snapshot even before the first MQTT message or while MQTT is disconnected. Unknown and last-known values remain visible according to the normal state rules; `/readyz` continues to report connection readiness separately. Clone a consistent snapshot under a short lock (or equivalent) and serialize it without holding up MQTT processing.

## Reliability and error handling

- Invalid startup configuration is fatal and produces a useful error and non-zero exit status.
- MQTT connection failures are retried with bounded backoff, such as 1 second increasing to 30 seconds.
- Subscribe again after every successful connection acknowledgement.
- An input topic can update every configured decoder that references that topic; apply all valid updates from that message before evaluating states or publishing commands.
- Use the custom logger for connect, disconnect, subscribe, rejected input, normalized fact update, state/priority transition, timer lifecycle, publish, and shutdown events without credentials.
- Do not panic on broker traffic or malformed input.
- If a required background task exits unexpectedly, stop the service with an error instead of leaving only the health server running.
- An explicit `online = false` updates the `online` application state but does not erase other facts.
- Missing optional inputs, including source health and all TPMS/fault inputs, do not prevent startup or readiness.

Input freshness is optional and disabled per input unless configured. Device-local watchdog fallback, controller heartbeat, logical jitter cooldowns, relay settling, freshness, and macro expiry are different policies with separate clocks; none may reset another's deadline as a side effect.

## Current-code cleanup required

The repository is an early scaffold and currently fails `cargo check`. Before adding MQTT behavior, the implementation agent should:

- Derive `Deserialize` for configuration file types that JSON5 loads.
- Finish `load_secrets_file`; it currently references an undefined value and returns the wrong type.
- Populate all `AppConfig` fields in `process_config`.
- Replace the topic vector with typed inputs (including optional parked), state settings, and output/rule/behavior mappings.
- Integrate the custom logger, replace application print/debug logging, and verify Kubernetes/instance enrichment before adding new logging sites.
- Model `state_settings` as a list of named entries, each with its own optional geofence and battery settings.
- Support missing/blank client IDs with generation and a single startup warning, and reject conflicting HTTP settings after environment overrides.
- Make `http.rs` use the processed port.
- Add the default-off state-endpoint setting, conditional route, and shared timestamped state snapshot.
- Replace placeholder health strings with styleguide JSON, shared build metadata, correlation handling, and HTTP 500 readiness failures; add bearer authentication and explicit diagnostic response types for `/state`.
- Add validation-only/dry-run execution modes, per-input freshness/coherent GPS pairing, raw/control-state filtering, retry/hold scheduling, optional device availability, and the heartbeat/watchdog deployment contract.
- Start HTTP, MQTT, timers, publisher, heartbeat, and any logging worker concurrently from `main.rs`, with startup logging outside the HTTP-enabled conditional.
- Remove the unnecessary parentheses around the `if` condition in `main.rs`.

## Tests and acceptance criteria

The implementation is complete when:

- `cargo fmt -- --check`, `cargo check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test` pass.
- Configuration defaults, recursive environment-reference expansion/precedence, both GPS modes, output-rule schemas, macro/hold/jitter bounds, heartbeat margins, and every validation rule have tests. Distinguish defined/empty/unset references, partial literal strings, and schema rejection after expansion; warn on unknown config key paths without warning for ordinary extra vehicle payload fields.
- Custom-logger integration tests capture boot logs with HTTP both on and off, matching version/build metadata, instance identity, generated-ID warnings, and sanitized errors. Verify supplied Kubernetes/instance variables appear on startup and ordinary/error events without duplication, default config does not mask them, explicit logger settings retain precedence, and noisy phase gates work. Check that application log sites no longer use ad hoc print/debug macros; run the logger's existing test suite when its integration changes.
- Client-ID tests cover explicit ID preservation, missing/empty/whitespace fallback, generated ID format, one startup warning, reuse across reconnects, regeneration on process restart, and generation failures. Use an injectable random source for deterministic tests; do not depend on chance to test uniqueness.
- Input tests cover shared topics, omitted optional inputs, named fault inputs, value normalization, unknown boolean values, malformed UTF-8/JSON, extra JSON/config fields, missing coordinates, and invalid numeric ranges.
- Haversine tests cover the target coordinate, inner/outer boundaries, the outer band, and points outside both radii.
- Multiple-location tests cover a match only at the second home, all locations false, unknown GPS, omitted/empty arrays, different radii per home, overlaps, and order-independent OR results for each location-based state. Test the OR evaluator with true/unknown and false/unknown combinations.
- Verify that an inner match at one home and an outer-band match at another can both be true; a non-matching entry must never overwrite a matching entry. A change of matching home with an unchanged combined state must produce no extra MQTT command, timer restart, or combined timestamp change; scheduled blink phases still run.
- Test per-entry validation, duplicate/empty names, and battery-only entries without geofences. Cover different thresholds and fault flags, the 20%/30% threshold example above, entries with battery settings omitted, and missing versus not-yet-received battery input. No state may combine a threshold from one entry with a fault flag from another.
- Every fixed application state has true, false, and unknown tests where applicable, including fault aggregation, `vehicle_fault_free`, and `source_healthy`.
- Source-health tests cover omitted configuration, no message yet, recognized true/false values, ignored empty/unrecognized values, optional expiry, and change-only steady publishing apart from explicit resynchronization. Changing source health must leave vehicle faults, online state, unrelated outputs, and HTTP readiness unchanged.
- Composite state tests cover unknown propagation.
- Charging tests cover active charging, completion while still plugged in (including a target below 100%), paused/waiting/no-power states, unplugging, resumed charging, and omitted connection/completion inputs. Inactivity must not imply completion, disconnection, or a vehicle fault.
- With the car inside and parked, a shared `Charging` -> `Complete` -> `Disconnected` sequence keeps the orange reminder off through completion and starts it only after disconnection. Green stays on through completion while connected. Verify one evaluation per shared message, unknown parked/connection leaving the reminder silent, and known non-parked data preventing it.
- Output tests prove first-known publishing, steady/phase transition publishing, duplicate suppression, true/false payload selection, one winning rule per relay, independent outputs, and reconnect resynchronization.
- With an injected clock and recording publisher, verify a full blink per X seconds under timely delivery, acceptance at X = 2 with a one-second hold, rejection below twice the hold/non-finite/invalid Y, and expiry at Y without requiring another MQTT message. The orange example must fall back to steady orange and remain there over simulated days of duplicate true messages.
- Cover priority preemption, expiry while preempted/offline, cancellation, rearming only after an admitted recognized non-match, stale callback suppression, delayed timer wakeups without bursts, and shutdown. Reconnects and read-only state requests must not restart the episode; missed/offline phases must not be replayed. A slow log sink must not stall the timer or MQTT loop.
- Jitter tests use an initial known baseline, then three admitted changes at t=0/1/2; a fourth pending value must wait until t=12, not t=10. Verify custom counts (including 1 and 5), custom cooldowns, quiet-period reset, latest-value coalescing, no deadline extension, automatic admission without another input, unknown propagation without rearming, and no budget consumption by blink phases or unchanged multi-home OR results.
- Relay timing tests cover every submission source: initial values, steady/phase changes, high-priority preemption, expiry/defaults, reconnect/device-online/heartbeat resync, and retries. At default hold, no two application-originated relay submissions may be less than one second apart; test configured longer holds, invalid shorter holds, coalescing during waits, ambiguous-attempt settling, and conservative startup/recovery waits. With X=2/Y=2/hold=1, anchor the first episode after settling and observe its ON then OFF phases before fallback.
- Freshness tests prove valid duplicates refresh per-fact receipt/deadline metadata without changing the value timestamp, malformed updates cannot prevent expiry, null/omitted expiry keeps last-known facts, expiry occurs without incoming messages, and recovery/unknown do not restart exhausted reminders. Retained receipt must not be presented as verified upstream measurement age.
- Split-GPS tests require a new value for both axes per committed pair, accept equal-valued fresh components, enforce receipt skew, and avoid transient mixed-generation coordinates. Cover an incomplete pair's non-extending deadline, freshness expiry while waiting, malformed components, skew failure/recovery, and atomic JSON updates.
- A failed steady submission must eventually retry successfully with **no new vehicle input**. Cover changing/unknown desired values while retrying, capped backoff, bounded queue/attempt timeout, cancellation during shutdown, no obsolete phase replay, and readiness/heartbeat suppression while control delivery is broken.
- Optional device availability tests cover no mapping, unknown at startup, invalidation on broker disconnect/new session, initial online, offline-to-online recovery while Carstate remains broker-connected, duplicate online suppression, deduplicated subscription acknowledgements, multiple outputs on one device, and coalescing simultaneous resynchronization reasons.
- Heartbeat tests cover disabled defaults, configured topic/payload/interval/timeout, first-ready pulse, invalid watchdog margins, QoS 0/non-retention independent of relay settings, current-output resynchronization before each pulse, and no backlog on reconnect. Test a heartbeat-specific submission failure followed by successful fresh recovery with unchanged input, and a pulse buffered immediately before disconnect/worker failure being discarded rather than replayed. A failed/stalled worker or publish path must stop pulses while retries can still recover; unknown vehicle facts alone must not. Simulate watchdog fallback without a connection event and prove the next healthy cycle restores current known outputs without restarting macros.
- Validation-only tests assert no MQTT/HTTP or remote logger connections, no file/device mutations, sanitized warnings with exact unknown-key paths, proper exit codes, and all normal cross-field checks. Dry-run tests use a recording publisher/transport spy to prove zero MQTT PUBLISH/Last Will actions, unique diagnostic ID override, real subscription/evaluation/timers, and separate simulated versus real submission metadata. Never run tests against a real configured relay.
- Provide an operator checklist for sole-writer upgrades and manual watchdog installation/verification. Test fallback on a dedicated test device or simulator by stopping controller pulses, removing broker access, and restarting the device; confirm the one-second physical hold separately if enforced device-side. Document that no software test proves the user's live watchdog is installed.
- An MQTT integration test proves that configured input messages result in the expected `POWER1` through `POWER4` command payloads.
- Probe tests verify the styleguide JSON fields, build metadata consistency, valid/missing/malformed correlation headers, and matching response header/body IDs. Liveness must perform no dependency calls and stay HTTP 200 during a broker outage; readiness must return HTTP 500 on any failed required check and HTTP 200 after connection/subscription recovery. Probe responses must omit internal runtime details and credentials.
- State-endpoint tests cover omitted/false settings returning 404 while HTTP is enabled, enabled JSON responses, and all facts/states present with nulls for unknowns. Enabling the endpoint while HTTP is disabled must fail validation before any network startup, both in file config and after environment overrides; both settings false must start without an HTTP listener.
- State snapshots include named per-entry location/battery states and combined results from the same update, with independent timestamps, omitted inapplicable entry states, and an empty `entry_states` object when `state_settings` is empty.
- Timestamp tests cover first-known false, actual changes, duplicate/equivalent messages, rejected input, unchanged derived results, and reconnect resynchronization. Reading a snapshot must not mutate state or publish commands, and concurrent reads must see facts and derived states from the same update.
- State diagnostics tests verify configured/generated client IDs, connection/subscription transitions, counters, output submission versus desired state, selected rules, timer deadlines/remaining duration/exhaustion, and runtime/build metadata. Blink phases change only the output's value-change timestamp, not the logical trigger state's timestamp. Require valid bearer authentication, reject missing/invalid tokens without snapshot data, fail enabled-endpoint startup without a token, and assert that secrets never appear in responses or authentication diagnostics.

## Out of scope for the MVP

- Multiple cars, tenants, or brokers in one process
- Calling vehicle APIs directly
- User-defined rules or an expression language
- Arbitrary nested JSON paths
- A database, history, dashboard, or web UI
- Physical relay-state acknowledgement/feedback, dimming, RGB color control, app-executed arbitrary scripts, or unbounded flashing; optional device availability, fixed-color steady/bounded-blink macros, and heartbeat-triggered device watchdog setup documentation are in scope
- Persisting facts, jitter budgets, or output/timer decisions across process restarts
- Distributed leader election, automatic watchdog installation, runtime config hot reload, and upstream historical timestamp reconciliation
