//! The commands operators send: what each asks of the printer, when it may,
//! and the log of who sent what.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};
use x2d_protocol::{Command, JobState, Outcome, Printer};

use crate::mqtt::Link;

/// Whether `command` makes sense for `printer` as it stands, or why not.
pub fn admissible(command: Command, printer: &Printer) -> Result<(), String> {
    if !printer.online {
        return Err("l'imprimante ne répond pas".to_owned());
    }
    let (allowed, why) = match command {
        Command::Light { .. } => (true, ""),
        Command::Pause => (matches!(printer.state, JobState::Printing | JobState::Preparing), "rien n'imprime"),
        Command::Resume => (printer.state == JobState::Paused, "l'impression n'est pas en pause"),
        Command::Stop => (printer.state.underway(), "rien n'imprime"),
    };
    if allowed { Ok(()) } else { Err(why.to_owned()) }
}

/// The section and body of the request that carries `command`.
fn request(command: Command) -> (&'static str, Map<String, Value>) {
    let body = match command {
        Command::Light { on } => json!({
            "command": "ledctrl",
            "led_node": "chamber_light",
            "led_mode": if on { "on" } else { "off" },
            "led_on_time": 500,
            "led_off_time": 500,
            "loop_times": 0,
            "interval_time": 0,
        }),
        Command::Pause => json!({ "command": "pause", "param": "" }),
        Command::Resume => json!({ "command": "resume", "param": "" }),
        Command::Stop => json!({ "command": "stop", "param": "" }),
    };
    let section = if matches!(command, Command::Light { .. }) { "system" } else { "print" };
    let Value::Object(body) = body else { unreachable!("the requests are objects") };
    (section, body)
}

/// Send `command` to the printer, and tell what became of it.
pub async fn send(link: &Link, command: Command) -> Outcome {
    let (section, body) = request(command);
    match link.request(section, body).await {
        Ok(answer) => outcome(&answer),
        Err(error) => Outcome::Failed(format!("{error:#}")),
    }
}

/// What the printer's `answer` says of a request.
fn outcome(answer: &Value) -> Outcome {
    let result = answer.get("result").and_then(Value::as_str).unwrap_or("");
    if result.eq_ignore_ascii_case("success") {
        return Outcome::Done;
    }
    let reason = answer.get("reason").and_then(Value::as_str).filter(|reason| !reason.is_empty()).unwrap_or(result);
    Outcome::Failed(if reason.is_empty() { "l'imprimante a refusé".to_owned() } else { reason.to_owned() })
}

/// The log of commands, one JSON line each.
pub struct Log {
    path: PathBuf,
}

impl Log {
    /// The log in `data`.
    pub fn new(data: &Path) -> Self {
        Self { path: data.join("commands.jsonl") }
    }

    /// Note that `who` sent `command`, and what became of it.
    pub fn note(&self, who: &str, command: Command, outcome: &Outcome) {
        let line = json!({ "time": crate::now(), "who": who, "command": command, "outcome": outcome });
        let written = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .and_then(|mut file| writeln!(file, "{line}"));
        if let Err(error) = written {
            tracing::warn!("cannot log a command: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_as_the_printer_expects() {
        let (section, body) = request(Command::Light { on: false });
        assert_eq!(
            (section, body["command"].as_str(), body["led_mode"].as_str()),
            ("system", Some("ledctrl"), Some("off"))
        );
        let (section, body) = request(Command::Stop);
        assert_eq!((section, body["command"].as_str()), ("print", Some("stop")));
    }

    #[test]
    fn commands_follow_the_state() {
        let printing = Printer { online: true, state: JobState::Printing, ..Printer::default() };
        let paused = Printer { online: true, state: JobState::Paused, ..Printer::default() };
        let idle = Printer { online: true, state: JobState::Idle, ..Printer::default() };
        assert!(admissible(Command::Pause, &printing).is_ok());
        assert!(admissible(Command::Resume, &printing).is_err());
        assert!(admissible(Command::Resume, &paused).is_ok());
        assert!(admissible(Command::Stop, &paused).is_ok());
        assert!(admissible(Command::Stop, &idle).is_err());
        assert!(admissible(Command::Light { on: true }, &idle).is_ok());
        assert!(admissible(Command::Light { on: true }, &Printer::default()).is_err());
        let unheard = Printer { online: false, ..printing };
        assert!(admissible(Command::Pause, &unheard).is_err(), "a printer unheard is not commanded");
    }

    #[test]
    fn answers_tell_their_outcome() {
        assert_eq!(outcome(&json!({ "result": "success" })), Outcome::Done);
        assert_eq!(outcome(&json!({ "result": "fail", "reason": "busy" })), Outcome::Failed("busy".into()));
        assert_eq!(outcome(&json!({})), Outcome::Failed("l'imprimante a refusé".into()));
    }
}
