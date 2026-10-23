//! What alerts and errors mean, in French, as Bambu Lab's own service says.
//!
//! Each code is asked once of `e.bambulab.com`, the service Bambu Studio
//! asks, and its text kept in `texts.json`, so that it is known at once
//! afterwards, even without the internet.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde_json::Value;
use tokio::sync::watch;
use x2d_protocol::{Alert, Printer};

/// The texts known, and those being asked for.
pub struct Texts {
    path: PathBuf,
    /// The serial number's first three characters, which name the model.
    model: String,
    known: Mutex<HashMap<String, String>>,
    asked: Mutex<HashSet<String>>,
    learned: watch::Sender<u64>,
    http: reqwest::Client,
}

/// How long to wait before asking again for a text that could not be had.
const RETRY_AFTER: Duration = Duration::from_secs(600);

impl Texts {
    /// The texts kept in `data`, for the printer of serial number `serial`.
    pub fn open(data: &Path, serial: &str) -> Arc<Self> {
        let path = data.join("texts.json");
        let known = std::fs::read(&path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        Arc::new(Self {
            path,
            model: serial.chars().take(3).collect(),
            known: Mutex::new(known),
            asked: Mutex::new(HashSet::new()),
            learned: watch::channel(0).0,
            http: reqwest::Client::builder().timeout(Duration::from_secs(15)).build().unwrap_or_default(),
        })
    }

    /// Changes whenever a text is learned.
    pub fn learned(&self) -> watch::Receiver<u64> {
        self.learned.subscribe()
    }

    /// Give `printer`'s alerts and error the texts known, and ask for those
    /// not known yet.
    pub fn fill(self: &Arc<Self>, printer: &mut Printer) {
        for alert in printer.alerts.iter_mut().chain(printer.error.as_mut()) {
            self.fill_alert(alert);
        }
    }

    fn fill_alert(self: &Arc<Self>, alert: &mut Alert) {
        if let Some(text) = lock(&self.known).get(&alert.code) {
            alert.text = Some(text.clone());
            return;
        }
        if lock(&self.asked).insert(alert.code.clone()) {
            tokio::spawn(Arc::clone(self).learn(alert.code.clone()));
        }
    }

    async fn learn(self: Arc<Self>, code: String) {
        match self.look_up(&code).await {
            Ok(Some(text)) => {
                let known = {
                    let mut known = lock(&self.known);
                    known.insert(code, text);
                    serde_json::to_vec_pretty(&*known)
                };
                if let Ok(bytes) = known
                    && let Err(error) = std::fs::write(&self.path, bytes)
                {
                    tracing::warn!("cannot keep the texts: {error}");
                }
                self.learned.send_modify(|count| *count += 1);
            }
            // The service has no text for it: it is not asked again until
            // the server restarts.
            Ok(None) => tracing::info!("Bambu Lab has no text for {code}"),
            Err(error) => {
                tracing::info!("no text for {code} yet: {error:#}");
                tokio::time::sleep(RETRY_AFTER).await;
                lock(&self.asked).remove(&code);
            }
        }
    }

    /// Ask Bambu Lab's service what `code` means, if it knows.
    async fn look_up(&self, code: &str) -> anyhow::Result<Option<String>> {
        let query = code.replace('_', "");
        let answer: Value = self
            .http
            .get("https://e.bambulab.com/query.php")
            .query(&[("lang", "fr"), ("d", self.model.as_str()), ("e", query.as_str())])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(text_in(&answer, &query))
    }
}

/// The text of `code` in the service's `answer`.
fn text_in(answer: &Value, code: &str) -> Option<String> {
    let data = answer.get("data")?.as_object()?;
    data.values().filter_map(|kind| kind.get("fr")?.as_array()).flatten().find_map(|entry| {
        let matches = entry.get("ecode")?.as_str()?.eq_ignore_ascii_case(code);
        let text = entry.get("intro")?.as_str()?.trim();
        (matches && !text.is_empty()).then(|| text.to_owned())
    })
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_service_answers_read() {
        let answer = serde_json::json!({ "result": 0, "data": { "device_error": { "ver": 1, "fr": [
            { "ecode": "0300400C", "intro": "La tâche a été annulée." }
        ] } } });
        assert_eq!(text_in(&answer, "0300400C").as_deref(), Some("La tâche a été annulée."));
        assert_eq!(text_in(&serde_json::json!({ "result": 201, "msg": "" }), "0300400C"), None);
    }
}
