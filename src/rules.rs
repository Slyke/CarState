use crate::{
    config::Config,
    inputs::Vehicle,
    model::{and, not, or, Location, State, States, Truth},
};
use std::collections::BTreeMap;
pub type Evaluation = (
    BTreeMap<State, Truth>,
    BTreeMap<String, BTreeMap<State, Truth>>,
);
pub fn distance(a: Location, b: Location) -> f64 {
    let lat = (b.latitude - a.latitude).to_radians();
    let lon = (b.longitude - a.longitude).to_radians();
    let h = (lat / 2.).sin().powi(2)
        + a.latitude.to_radians().cos() * b.latitude.to_radians().cos() * (lon / 2.).sin().powi(2);
    6_371_000. * 2. * h.clamp(0., 1.).sqrt().asin()
}
pub fn evaluate(c: &Config, v: &Vehicle) -> Evaluation {
    evaluate_with_history(c, v, &BTreeMap::new())
}
/// Apply each geofence's exit buffer to its previous membership, before deriving
/// composite states. Missing/unknown history uses the original entry thresholds;
/// an unknown location clears membership when the engine stores these results.
pub fn evaluate_with_history(
    c: &Config,
    v: &Vehicle,
    previous: &BTreeMap<String, States>,
) -> Evaluation {
    use State::*;
    let mut combined: BTreeMap<_, _> = State::ALL.into_iter().map(|s| (s, None)).collect();
    for (state, name) in [
        (Charging, "charging"),
        (PluggedIn, "plugged_in"),
        (ChargeComplete, "charge_complete"),
        (Parked, "parked"),
        (Locked, "locked"),
        (Online, "online"),
        (SourceHealthy, "source_healthy"),
    ] {
        combined.insert(state, v.boolean(name));
    }
    let charging = v.boolean("charging");
    let plugged = v.boolean("plugged_in");
    let parked = v.boolean("parked");
    let complete = v.boolean("charge_complete");
    let mut entries = BTreeMap::new();
    let mut faults: Vec<_> = v.faults.values().map(|f| f.current.value).collect();
    for e in &c.state_settings {
        let mut states = BTreeMap::new();
        if let (Some(latitude), Some(longitude), Some(inner), Some(outer)) = (
            e.target_latitude,
            e.target_longitude,
            e.inner_radius_meters,
            e.outer_radius_meters,
        ) {
            let d = v.location.current.value.map(|l| {
                distance(
                    l,
                    Location {
                        latitude,
                        longitude,
                    },
                )
            });
            let was_inside = |state| {
                previous
                    .get(&e.name)
                    .and_then(|states| states.get(&state))
                    .and_then(|value| value.value)
                    == Some(true)
            };
            let exit_buffer = |state| {
                if was_inside(state) {
                    e.hysteresis_meters
                } else {
                    0.
                }
            };
            let inside = d.map(|d| d < inner + exit_buffer(LocationInner));
            let within = d.map(|d| d <= outer + exit_buffer(LocationWithinOuter));
            for (state, value) in [
                (LocationWithinOuter, within),
                (LocationOuterBand, and([within, not(inside)])),
                (LocationInner, inside),
                (LocationInnerNotCharging, and([inside, not(charging)])),
                (LocationInnerAndCharging, and([inside, charging])),
                (LocationInnerAndPluggedIn, and([inside, plugged])),
                (LocationInnerNotPluggedIn, and([inside, not(plugged)])),
                (LocationInnerNotParked, and([inside, not(parked)])),
                (
                    LocationInnerParkedNotPluggedIn,
                    and([inside, parked, not(plugged)]),
                ),
                (
                    LocationInnerPluggedInNotCharging,
                    and([inside, plugged, not(charging)]),
                ),
                (
                    LocationInnerChargeComplete,
                    and([inside, plugged, not(charging), complete]),
                ),
            ] {
                states.insert(state, value);
            }
        }
        if let Some(threshold) = e.battery_low_percent {
            let low = v.battery.current.value.map(|b| b <= threshold);
            states.insert(BatteryLow, low);
            if e.battery_low_is_fault && c.inputs.battery.is_some() {
                faults.push(low);
            }
        }
        entries.insert(e.name.clone(), states);
    }
    for state in State::ALL {
        let values: Vec<_> = entries
            .values()
            .filter_map(|e| e.get(&state).copied())
            .collect();
        if !values.is_empty() {
            combined.insert(state, or(values));
        }
    }
    let fault = or(faults);
    combined.insert(VehicleFault, fault);
    combined.insert(VehicleFaultFree, not(fault));
    (combined, entries)
}
/// Whether the configured dependencies can ever supply a known trigger.
pub fn possible(state: State, c: &Config) -> bool {
    use State::*;
    let location =
        c.inputs.location.is_some() && c.state_settings.iter().any(|e| e.target_latitude.is_some());
    match state {
        Charging => c.inputs.charging.is_some(),
        PluggedIn => c.inputs.plugged_in.is_some(),
        ChargeComplete => c.inputs.charge_complete.is_some(),
        Parked => c.inputs.parked.is_some(),
        Locked => c.inputs.locked.is_some(),
        Online => c.inputs.online.is_some(),
        SourceHealthy => c.inputs.source_healthy.is_some(),
        BatteryLow => {
            c.inputs.battery.is_some()
                && c.state_settings
                    .iter()
                    .any(|e| e.battery_low_percent.is_some())
        }
        VehicleFault | VehicleFaultFree => {
            !c.inputs.faults.is_empty()
                || (c.inputs.battery.is_some()
                    && c.state_settings
                        .iter()
                        .any(|e| e.battery_low_is_fault && e.battery_low_percent.is_some()))
        }
        // A known outside result can resolve any composite false even if another fact is omitted.
        _ => location,
    }
}
