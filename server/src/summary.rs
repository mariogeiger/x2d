//! The printer told in a few lines of text, as the bot sends it.

use x2d_protocol::{Heater, Printer, stage_label};

/// One line: whether the printer is heard, where it stands, and how far its
/// print is.
pub fn headline(printer: &Printer, now: i64) -> String {
    if !printer.online {
        let since = if printer.reported_at > 0 {
            format!(" depuis {}", duration((now - printer.reported_at).max(0) / 60))
        } else {
            String::new()
        };
        return format!("X2D ne répond pas{since}.");
    }
    let mut line = format!("X2D : {}", printer.state.label());
    if let Some(stage) = printer.stage.and_then(stage_label).filter(|stage| *stage != printer.state.label()) {
        line.push_str(&format!(", {stage}"));
    }
    if let Some(job) = &printer.job {
        line.push_str(&format!(" · « {} » {} %", job.name, job.percent));
    }
    line
}

/// The printer in a few lines: its print, its heaters, its AMS, its alerts.
pub fn summary(printer: &Printer, now: i64, origin: &str) -> String {
    let mut lines = vec![headline(printer, now)];
    if let Some(job) = &printer.job
        && printer.state.underway()
    {
        lines.push(format!(
            "Reste {}, couche {} / {}.",
            duration(i64::from(job.remaining_minutes)),
            job.layer,
            job.total_layers
        ));
    }
    let heaters: Vec<String> = printer
        .nozzles
        .iter()
        .map(|nozzle| {
            let active = if nozzle.active && printer.nozzles.len() > 1 { " (active)" } else { "" };
            format!("{}{active} : {}", nozzle.label(printer.nozzles.len()), heater(nozzle.heater))
        })
        .chain(printer.bed.map(|bed| format!("Plateau : {}", heater(bed))))
        .chain(printer.chamber.map(|chamber| format!("Chambre : {}", heater(chamber))))
        .collect();
    if !heaters.is_empty() {
        lines.push(heaters.join(" · "));
    }
    for unit in &printer.ams {
        let mut line = unit.label();
        if let Some(humidity) = unit.humidity {
            line.push_str(&format!(" : {humidity} % d'humidité"));
        }
        if let Some(minutes) = unit.drying_minutes {
            line.push_str(&format!(", séchage encore {}", duration(i64::from(minutes))));
        }
        lines.push(line);
    }
    for alert in &printer.alerts {
        lines.push(format!("⚠️ {} ({})", alert.text.as_deref().unwrap_or("alerte sans description"), alert.code));
    }
    if let Some(error) = &printer.error {
        lines.push(format!("❌ {} ({})", error.text.as_deref().unwrap_or("erreur sans description"), error.code));
    }
    lines.push(origin.to_owned());
    lines.join("\n")
}

/// A heater's temperatures: where it is, and where it heads unless off.
fn heater(heater: Heater) -> String {
    if heater.target > 0.0 {
        format!("{:.0} / {:.0} °C", heater.current, heater.target)
    } else {
        format!("{:.0} °C", heater.current)
    }
}

/// `minutes` in hours and minutes.
fn duration(minutes: i64) -> String {
    match minutes {
        60.. => format!("{} h {:02}", minutes / 60, minutes % 60),
        _ => format!("{minutes} min"),
    }
}

#[cfg(test)]
mod tests {
    use x2d_protocol::{Alert, AmsUnit, Job, JobState, Nozzle};

    use super::*;

    #[test]
    fn a_print_reads_in_a_few_lines() {
        let printer = Printer {
            online: true,
            reported_at: 100,
            state: JobState::Printing,
            stage: Some(0),
            job: Some(Job {
                name: "box".into(),
                percent: 83,
                remaining_minutes: 76,
                layer: 58,
                total_layers: 165,
                ..Job::default()
            }),
            nozzles: vec![
                Nozzle { id: 0, heater: Heater { current: 42.0, target: 0.0 }, ..Nozzle::default() },
                Nozzle { id: 1, heater: Heater { current: 220.0, target: 220.0 }, active: true, ..Nozzle::default() },
            ],
            bed: Some(Heater { current: 55.0, target: 55.0 }),
            chamber: Some(Heater { current: 32.0, target: 0.0 }),
            ams: vec![AmsUnit { id: 0, humidity: Some(36), ..AmsUnit::default() }],
            alerts: vec![Alert { code: "0300_2E00_0003_0001".into(), text: Some("Recalibrer.".into()) }],
            ..Printer::default()
        };
        assert_eq!(
            summary(&printer, 100, "https://x2d"),
            "X2D : impression · « box » 83 %\n\
             Reste 1 h 16, couche 58 / 165.\n\
             Buse droite : 42 °C · Buse gauche (active) : 220 / 220 °C · Plateau : 55 / 55 °C · Chambre : 32 °C\n\
             AMS A : 36 % d'humidité\n\
             ⚠️ Recalibrer. (0300_2E00_0003_0001)\n\
             https://x2d"
        );
    }

    #[test]
    fn an_unheard_printer_says_since_when() {
        let printer = Printer { online: false, reported_at: 1_000, ..Printer::default() };
        assert_eq!(headline(&printer, 1_000 + 7_500), "X2D ne répond pas depuis 2 h 05.");
        assert_eq!(headline(&Printer::default(), 5), "X2D ne répond pas.");
    }
}
