//! Notifications on Telegram: the end of a print, well or not, and each new
//! health alert, sent by the bot to the chat of the configuration.
//!
//! What was last notified is kept in `notified.json`, so that a restart
//! neither repeats a notification nor misses an end that came meanwhile.
//! The first time, the printer's state is taken as already known.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use x2d_protocol::{JobState, Printer};

use crate::camera::Camera;
use crate::config::TelegramConfig;

/// A bot and the chat it writes to.
pub struct Telegram {
    http: reqwest::Client,
    api: String,
    chat: i64,
}

impl Telegram {
    /// The bot of `config`.
    pub fn new(config: &TelegramConfig) -> Self {
        Self {
            http: reqwest::Client::builder().timeout(Duration::from_secs(30)).build().unwrap_or_default(),
            api: format!("https://api.telegram.org/bot{}", config.token),
            chat: config.chat,
        }
    }

    /// Write `text` to the chat.
    pub async fn text(&self, text: &str) -> anyhow::Result<()> {
        let body = json!({ "chat_id": self.chat, "text": text, "link_preview_options": { "is_disabled": true } });
        self.answered(self.http.post(format!("{}/sendMessage", self.api)).json(&body).send().await).await
    }

    /// Send the JPEG picture `jpeg` to the chat, with `caption`.
    pub async fn photo(&self, jpeg: Vec<u8>, caption: &str) -> anyhow::Result<()> {
        let form = reqwest::multipart::Form::new()
            .text("chat_id", self.chat.to_string())
            .text("caption", caption.to_owned())
            .part("photo", reqwest::multipart::Part::bytes(jpeg).file_name("x2d.jpg").mime_str("image/jpeg")?);
        self.answered(self.http.post(format!("{}/sendPhoto", self.api)).multipart(form).send().await).await
    }

    async fn answered(&self, response: reqwest::Result<reqwest::Response>) -> anyhow::Result<()> {
        // The URL holds the token: errors are told without it.
        let response =
            response.map_err(|error| anyhow::anyhow!("Telegram cannot be reached: {}", error.without_url()))?;
        let answer: Value = response
            .json()
            .await
            .map_err(|error| anyhow::anyhow!("Telegram answered no JSON: {}", error.without_url()))?;
        if answer.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "Telegram refused: {}",
                answer.get("description").and_then(Value::as_str).unwrap_or("?")
            ))
        }
    }
}

/// What was last notified.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Notified {
    state: JobState,
    alerts: HashSet<String>,
}

/// What to say of a change, and whether to show the camera with it.
#[derive(Debug, PartialEq, Eq)]
struct Notice {
    text: String,
    picture: bool,
}

/// How long a new alert waits for its text before it is told without.
const TEXT_WITHIN: Duration = Duration::from_secs(30);

/// Notify the chat of what changes on the printer, for as long as the server
/// runs.
pub async fn notify(
    telegram: Telegram,
    mut printer: tokio::sync::watch::Receiver<Arc<Printer>>,
    camera: Camera,
    path: PathBuf,
    origin: String,
) {
    let mut notified: Option<Notified> =
        std::fs::read(&path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let mut seen: HashMap<String, Instant> = HashMap::new();
    loop {
        let now = Arc::clone(&printer.borrow_and_update());
        if now.online && now.state != JobState::Unknown {
            let last = notified.clone().unwrap_or_else(|| Notified { state: now.state.clone(), alerts: codes(&now) });
            let (notices, next) = changes(&last, &now, &mut seen, &origin);
            for notice in notices {
                let sent = match notice.picture {
                    true => match tokio::time::timeout(Duration::from_secs(30), camera.picture()).await {
                        Ok(Ok(jpeg)) => telegram.photo(jpeg, &notice.text).await,
                        _ => telegram.text(&notice.text).await,
                    },
                    false => telegram.text(&notice.text).await,
                };
                if let Err(error) = sent {
                    tracing::warn!("cannot notify: {error:#}");
                }
            }
            if notified.as_ref() != Some(&next) {
                if let Err(error) = keep(&path, &next) {
                    tracing::warn!("cannot keep what was notified: {error:#}");
                }
                notified = Some(next);
            }
        }
        // Wake at least every few seconds, so that an alert whose text does
        // not come is told all the same.
        if tokio::time::timeout(Duration::from_secs(5), printer.changed()).await.is_ok_and(|changed| changed.is_err()) {
            return;
        }
    }
}

fn keep(path: &std::path::Path, notified: &Notified) -> anyhow::Result<()> {
    std::fs::write(path, serde_json::to_vec(notified)?).with_context(|| format!("cannot write {}", path.display()))
}

fn codes(printer: &Printer) -> HashSet<String> {
    printer.alerts.iter().map(|alert| alert.code.clone()).collect()
}

/// What to notify between `last` and `now`, and what is notified after.
/// `seen` remembers when each alert not yet told was first seen.
fn changes(
    last: &Notified,
    now: &Printer,
    seen: &mut HashMap<String, Instant>,
    origin: &str,
) -> (Vec<Notice>, Notified) {
    let mut notices = Vec::new();
    let name = now.job.as_ref().map_or_else(|| "sans nom".to_owned(), |job| job.name.clone());
    if last.state.underway() && now.state == JobState::Finished {
        notices.push(Notice { text: format!("✅ L'impression « {name} » est terminée.\n{origin}"), picture: true });
    }
    if last.state.underway() && now.state == JobState::Failed {
        let why = now.error.as_ref().map_or_else(String::new, |error| {
            format!(" : {} ({})", error.text.as_deref().unwrap_or("erreur"), error.code)
        });
        notices
            .push(Notice { text: format!("❌ L'impression « {name} » a échoué{why}.\n{origin}"), picture: true });
    }
    let present = codes(now);
    seen.retain(|code, _| present.contains(code));
    let mut alerts: HashSet<String> = last.alerts.intersection(&present).cloned().collect();
    for alert in &now.alerts {
        if alerts.contains(&alert.code) {
            continue;
        }
        let first_seen = *seen.entry(alert.code.clone()).or_insert_with(Instant::now);
        if alert.text.is_none() && first_seen.elapsed() < TEXT_WITHIN {
            continue;
        }
        let text = alert.text.as_deref().unwrap_or("alerte sans description");
        notices.push(Notice { text: format!("⚠️ X2D : {text}\n({})", alert.code), picture: false });
        alerts.insert(alert.code.clone());
        seen.remove(&alert.code);
    }
    (notices, Notified { state: now.state.clone(), alerts })
}

#[cfg(test)]
mod tests {
    use x2d_protocol::{Alert, Job};

    use super::*;

    fn printer(state: JobState, alerts: &[(&str, Option<&str>)]) -> Printer {
        Printer {
            online: true,
            state,
            job: Some(Job { name: "box".into(), ..Job::default() }),
            alerts: alerts
                .iter()
                .map(|(code, text)| Alert { code: (*code).into(), text: text.map(str::to_owned) })
                .collect(),
            ..Printer::default()
        }
    }

    #[test]
    fn the_end_of_a_print_is_told_once() {
        let mut seen = HashMap::new();
        let printing = Notified { state: JobState::Printing, alerts: HashSet::new() };
        let (notices, next) = changes(&printing, &printer(JobState::Finished, &[]), &mut seen, "https://x2d");
        assert_eq!(
            notices,
            [Notice { text: "✅ L'impression « box » est terminée.\nhttps://x2d".into(), picture: true }]
        );
        let (notices, _) = changes(&next, &printer(JobState::Finished, &[]), &mut seen, "https://x2d");
        assert!(notices.is_empty());
    }

    #[test]
    fn a_new_alert_waits_for_its_text_and_is_told_once() {
        let mut seen = HashMap::new();
        let quiet = Notified { state: JobState::Printing, alerts: HashSet::new() };
        let (notices, next) =
            changes(&quiet, &printer(JobState::Printing, &[("0300_2E00_0003_0001", None)]), &mut seen, "");
        assert!(notices.is_empty(), "the text may still come");
        let alerting = printer(JobState::Printing, &[("0300_2E00_0003_0001", Some("Recalibrer."))]);
        let (notices, next) = changes(&next, &alerting, &mut seen, "");
        assert_eq!(notices, [Notice { text: "⚠️ X2D : Recalibrer.\n(0300_2E00_0003_0001)".into(), picture: false }]);
        let (notices, next) = changes(&next, &alerting, &mut seen, "");
        assert!(notices.is_empty());
        let (_, next) = changes(&next, &printer(JobState::Printing, &[]), &mut seen, "");
        let (notices, _) = changes(&next, &alerting, &mut seen, "");
        assert_eq!(notices.len(), 1, "an alert that comes back is told again");
    }
}
