//! What the printer's merged reports mean: the [`Printer`] a page shows.
//!
//! The reports pack and name things as the firmware does. The dual-nozzle
//! printers (H2D, X2D) report their hotends, bed and chamber under `device`,
//! each temperature packed as `target << 16 | current` in °C, while older
//! printers report `nozzle_temper` and its kin at the top; both are read.

use serde_json::Value;
use x2d_protocol::{Alert, AmsUnit, Fans, Heater, Job, JobState, Nozzle, Printer, Slot, Tray};

use crate::mqtt::Reports;

/// The printer that `reports` describe.
pub fn printer(reports: &Reports) -> Printer {
    let empty = Value::Null;
    let print = reports.state.get("print").unwrap_or(&empty);
    let state = print.get("gcode_state").and_then(Value::as_str).map_or(JobState::Unknown, JobState::from_report);
    Printer {
        online: reports.online,
        reported_at: reports.received_at,
        stage: int(print, "stg_cur").filter(|&stage| (0..255).contains(&stage)).map(|stage| stage as i32),
        job: job(print, &state),
        nozzles: nozzles(print),
        bed: packed(print.pointer("/device/bed/info/temp"))
            .or_else(|| heater(print, "bed_temper", "bed_target_temper")),
        chamber: packed(print.pointer("/device/ctc/info/temp")).or_else(|| heater(print, "chamber_temper", "")),
        ams: print
            .pointer("/ams/ams")
            .and_then(Value::as_array)
            .map_or_else(Vec::new, |units| units.iter().filter_map(ams_unit).collect()),
        external: external(print),
        alerts: alerts(print),
        error: int(print, "print_error")
            .filter(|&code| code != 0)
            .map(|code| Alert { code: format!("{:04X}_{:04X}", (code >> 16) & 0xFFFF, code & 0xFFFF), text: None }),
        fans: Fans {
            part: fan(print, "cooling_fan_speed"),
            aux: fan(print, "big_fan1_speed"),
            chamber: fan(print, "big_fan2_speed"),
            heatbreak: fan(print, "heatbreak_fan_speed"),
        },
        speed: int(print, "spd_lvl").and_then(|level| u8::try_from(level).ok()),
        wifi_dbm: print
            .get("wifi_signal")
            .and_then(Value::as_str)
            .and_then(|signal| signal.trim_end_matches("dBm").parse().ok()),
        light: print.get("lights_report").and_then(Value::as_array).and_then(|lights| {
            lights
                .iter()
                .find(|light| light.get("node").and_then(Value::as_str) == Some("chamber_light"))
                .map(|light| light.get("mode").and_then(Value::as_str) != Some("off"))
        }),
        state,
    }
}

/// The integer at `key`, which reports write as a number or as a string.
fn int(object: &Value, key: &str) -> Option<i64> {
    number(object.get(key)?)
}

fn number(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number.as_i64().or_else(|| number.as_f64().map(|float| float as i64)),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

fn float(object: &Value, key: &str) -> Option<f32> {
    match object.get(key)? {
        Value::Number(number) => number.as_f64().map(|float| float as f32),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

fn text(object: &Value, key: &str) -> Option<String> {
    object.get(key).and_then(Value::as_str).map(str::trim).filter(|text| !text.is_empty()).map(str::to_owned)
}

/// A temperature packed as `target << 16 | current`.
fn packed(value: Option<&Value>) -> Option<Heater> {
    let packed = number(value?)?;
    Some(Heater { current: (packed & 0xFFFF) as f32, target: ((packed >> 16) & 0xFFFF) as f32 })
}

fn heater(print: &Value, current: &str, target: &str) -> Option<Heater> {
    Some(Heater { current: float(print, current)?, target: float(print, target).unwrap_or(0.0) })
}

/// A fan's speed, which reports give in sixteenths, in % rounded to tens as
/// the printer's screen shows it.
fn fan(print: &Value, key: &str) -> Option<u8> {
    let gear = int(print, key)?;
    u8::try_from((gear * 100 + 75) / 150 * 10).ok()
}

fn job(print: &Value, state: &JobState) -> Option<Job> {
    let name = text(print, "subtask_name")?;
    if matches!(state, JobState::Idle | JobState::Unknown) {
        return None;
    }
    let count = |key| int(print, key).and_then(|value| u32::try_from(value).ok()).unwrap_or(0);
    Some(Job {
        name,
        percent: u8::try_from(int(print, "mc_percent").unwrap_or(0).clamp(0, 100)).unwrap_or(0),
        remaining_minutes: count("mc_remaining_time"),
        layer: count("layer_num"),
        total_layers: count("total_layer_num"),
        plate: count("plate_idx").max(1),
        thumbnail: None,
    })
}

/// The slot a nozzle's `snow` field names, `unit << 8 | tray`, or none: a
/// tray of 255 names none, as `0xFEFF` once the filament is unloaded.
fn slot(code: i64) -> Option<Slot> {
    let (unit, tray) = (u8::try_from(code >> 8).ok()?, u8::try_from(code & 0xFF).ok()?);
    (tray != 0xFF).then_some(Slot { unit, tray })
}

fn nozzles(print: &Value) -> Vec<Nozzle> {
    let described = |id: i64| {
        print
            .pointer("/device/nozzle/info")
            .and_then(Value::as_array)
            .and_then(|nozzles| nozzles.iter().find(|nozzle| int(nozzle, "id") == Some(id)))
    };
    if let Some(extruders) = print.pointer("/device/extruder/info").and_then(Value::as_array) {
        let active = print.pointer("/device/extruder/state").and_then(number).map(|state| (state >> 4) & 0xF);
        return extruders
            .iter()
            .filter_map(|extruder| {
                let id = int(extruder, "id")?;
                let nozzle = described(id);
                Some(Nozzle {
                    id: u8::try_from(id).ok()?,
                    heater: packed(extruder.get("temp"))?,
                    active: active == Some(id),
                    diameter: nozzle.and_then(|nozzle| float(nozzle, "diameter")),
                    kind: nozzle.and_then(|nozzle| text(nozzle, "type")),
                    loaded: int(extruder, "snow").and_then(slot),
                })
            })
            .collect();
    }
    let Some(heater) = heater(print, "nozzle_temper", "nozzle_target_temper") else { return Vec::new() };
    let loaded = print.pointer("/ams/tray_now").and_then(number).and_then(|tray| match tray {
        254 | 255 => Some(Slot { unit: tray as u8, tray: 0 }),
        0..=127 => Some(Slot { unit: (tray / 4) as u8, tray: (tray % 4) as u8 }),
        _ => None,
    });
    vec![Nozzle {
        id: 0,
        heater,
        active: true,
        diameter: float(print, "nozzle_diameter"),
        kind: text(print, "nozzle_type"),
        loaded,
    }]
}

/// The colour `RRGGBBAA` as `RRGGBB`.
fn color(rgba: &str) -> Option<String> {
    let rgb = rgba.get(..6)?;
    rgb.bytes().all(|byte| byte.is_ascii_hexdigit()).then(|| rgb.to_ascii_uppercase())
}

fn tray(object: &Value, slot: Slot) -> Tray {
    let material = text(object, "tray_type");
    let mut colors: Vec<String> = object
        .get("cols")
        .and_then(Value::as_array)
        .map(|colors| colors.iter().filter_map(Value::as_str).filter_map(color).collect())
        .unwrap_or_default();
    if colors.is_empty() {
        colors.extend(object.get("tray_color").and_then(Value::as_str).and_then(color));
    }
    if material.is_none() {
        colors.clear();
    }
    Tray {
        slot,
        name: text(object, "tray_sub_brands").filter(|_| material.is_some()),
        remain: int(object, "remain").filter(|_| material.is_some()).and_then(|remain| u8::try_from(remain).ok()),
        material,
        colors,
    }
}

fn ams_unit(unit: &Value) -> Option<AmsUnit> {
    let id = u8::try_from(int(unit, "id")?).ok()?;
    let trays = unit.get("tray").and_then(Value::as_array).map_or_else(Vec::new, |trays| {
        trays
            .iter()
            .filter_map(|object| {
                let index = u8::try_from(int(object, "id")?).ok()?;
                Some(tray(object, Slot { unit: id, tray: index }))
            })
            .collect()
    });
    Some(AmsUnit {
        id,
        humidity: int(unit, "humidity_raw").and_then(|humidity| u8::try_from(humidity).ok()),
        temperature: float(unit, "temp"),
        drying_minutes: int(unit, "dry_time")
            .filter(|&minutes| minutes > 0)
            .and_then(|minutes| u32::try_from(minutes).ok()),
        trays,
    })
}

fn external(print: &Value) -> Vec<Tray> {
    let holders = print
        .get("vir_slot")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| print.get("vt_tray").into_iter().cloned().collect());
    holders
        .iter()
        .filter_map(|holder| {
            let unit = u8::try_from(int(holder, "id")?).ok().filter(|&unit| unit >= 254)?;
            let tray = tray(holder, Slot { unit, tray: 0 });
            tray.material.is_some().then_some(tray)
        })
        .collect()
}

/// The health alerts, each `attr` and `code` written as Bambu Lab writes
/// them: four groups of four hexadecimal digits.
fn alerts(print: &Value) -> Vec<Alert> {
    let Some(alerts) = print.get("hms").and_then(Value::as_array) else { return Vec::new() };
    alerts
        .iter()
        .filter_map(|alert| {
            let attr = int(alert, "attr")?;
            let code = int(alert, "code")?;
            Some(Alert {
                code: format!(
                    "{:04X}_{:04X}_{:04X}_{:04X}",
                    (attr >> 16) & 0xFFFF,
                    attr & 0xFFFF,
                    (code >> 16) & 0xFFFF,
                    code & 0xFFFF
                ),
                text: None,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn reports(state: Value) -> Reports {
        Reports { online: true, received_at: 1_792_755_255, state: Arc::new(state) }
    }

    fn printing() -> Value {
        serde_json::from_str(include_str!("../fixtures/x2d-printing.json")).unwrap()
    }

    #[test]
    fn an_x2d_printing_reads_as_seen_on_its_screen() {
        let printer = printer(&reports(printing()));
        assert_eq!(printer.state, JobState::Printing);
        assert_eq!(printer.stage, Some(0));
        let job = printer.job.unwrap();
        assert_eq!((job.name.as_str(), job.percent, job.remaining_minutes), ("box", 83, 16));
        assert_eq!((job.layer, job.total_layers, job.plate), (58, 165, 1));
        assert_eq!(printer.nozzles.len(), 2);
        let [right, left] = &printer.nozzles[..] else { unreachable!() };
        assert_eq!(right.heater, Heater { current: 42.0, target: 0.0 });
        assert_eq!(left.heater, Heater { current: 220.0, target: 220.0 });
        assert!(left.active && !right.active);
        assert_eq!(left.loaded, Some(Slot { unit: 0, tray: 1 }));
        assert_eq!(right.loaded, None);
        assert_eq!(left.diameter, Some(0.4));
        assert_eq!(printer.bed, Some(Heater { current: 55.0, target: 55.0 }));
        assert_eq!(printer.chamber, Some(Heater { current: 32.0, target: 0.0 }));
        let ams = &printer.ams[0];
        assert_eq!((ams.label(), ams.humidity, ams.temperature), ("AMS A".to_owned(), Some(36), Some(29.1)));
        assert_eq!(ams.trays.len(), 4);
        assert_eq!(ams.trays[1].colors, ["9D432C"]);
        assert_eq!(ams.trays[1].name.as_deref(), Some("PLA Basic"));
        assert_eq!(ams.trays[3].material, None);
        assert!(printer.external.is_empty());
        assert_eq!(printer.fans, Fans { part: Some(100), aux: Some(70), chamber: Some(60), heatbreak: Some(0) });
        assert_eq!((printer.speed, printer.wifi_dbm, printer.light), (Some(2), Some(-63), Some(true)));
        assert!(printer.alerts.is_empty() && printer.error.is_none());
    }

    #[test]
    fn an_unloaded_nozzle_holds_no_filament() {
        assert_eq!(slot(0xFEFF), None, "as the X2D reports once a print unloaded its filament");
        assert_eq!(slot(0xFFFF), None);
        assert_eq!(slot(0xFE00), Some(Slot { unit: 254, tray: 0 }));
        assert_eq!(slot(0x0102), Some(Slot { unit: 1, tray: 2 }));
    }

    #[test]
    fn alerts_and_errors_are_written_as_bambu_lab_writes_them() {
        let mut state = printing();
        state["print"]["hms"] = serde_json::json!([{ "attr": 0x0300_2E00_u32, "code": 0x0003_0001 }]);
        state["print"]["print_error"] = serde_json::json!(0x0500_4024);
        let printer = printer(&reports(state));
        assert_eq!(printer.alerts[0].code, "0300_2E00_0003_0001");
        assert_eq!(printer.error.unwrap().code, "0500_4024");
    }

    #[test]
    fn a_single_nozzle_printer_reads_from_the_top_fields() {
        let state = serde_json::json!({ "print": {
            "gcode_state": "IDLE", "nozzle_temper": 24.5, "nozzle_target_temper": 0,
            "bed_temper": 23.0, "bed_target_temper": 0, "chamber_temper": 22.0,
            "ams": { "tray_now": "5" }, "stg_cur": 255, "subtask_name": "old"
        } });
        let printer = printer(&reports(state));
        assert_eq!(printer.nozzles[0].heater, Heater { current: 24.5, target: 0.0 });
        assert_eq!(printer.nozzles[0].loaded, Some(Slot { unit: 1, tray: 1 }));
        assert_eq!(printer.chamber, Some(Heater { current: 22.0, target: 0.0 }));
        assert_eq!((printer.stage, printer.job), (None, None));
    }
}
