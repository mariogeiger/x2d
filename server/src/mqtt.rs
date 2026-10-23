//! The printer's MQTT link: one TLS connection, kept up for as long as the
//! server runs, that hears every report and carries every request.
//!
//! The printer publishes its state as `push_status` reports on
//! `device/<serial>/report`, a full one when asked with `pushall` and then
//! the parts that change. Merged, they make the printer's whole state, which
//! [`Link::reports`] shares. A request goes to `device/<serial>/request` with
//! a sequence number, and the printer answers it on the report topic with
//! the same number and a `result`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, anyhow};
use rumqttc::{AsyncClient, Event, EventLoop, MqttOptions, Packet, QoS, TlsConfiguration, Transport};
use serde_json::{Map, Value, json};
use tokio::sync::{oneshot, watch};

use crate::config::PrinterConfig;
use crate::tls;

/// The printer's merged state, as last heard.
#[derive(Clone, Debug, Default)]
pub struct Reports {
    /// Whether the link is up.
    pub online: bool,
    /// When the last report came, in Unix seconds; 0 if none did.
    pub received_at: i64,
    /// Every report merged, under their sections such as `print`.
    pub state: Arc<Value>,
}

/// The link to the printer.
pub struct Link {
    client: AsyncClient,
    request_topic: String,
    reports: watch::Receiver<Reports>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Value>>>,
    sequence: AtomicU64,
}

/// How long the printer has to answer a request.
const ANSWER_WITHIN: Duration = Duration::from_secs(10);
/// How often the whole state is asked for again, lest a lost part linger.
const RESYNC_EVERY: Duration = Duration::from_secs(300);
/// The first sequence number of requests: far above the printer's own
/// counters, so that its reports are never taken for answers.
const FIRST_SEQUENCE: u64 = 3_000_000_000;

impl Link {
    /// Connect to the printer, and keep connecting for as long as the server
    /// runs.
    pub fn start(printer: &PrinterConfig) -> anyhow::Result<Arc<Self>> {
        let mut id = [0_u8; 6];
        getrandom::fill(&mut id).map_err(|error| anyhow!("no randomness: {error}"))?;
        let client_id = format!("x2d-{}", id.iter().map(|byte| format!("{byte:02x}")).collect::<String>());
        let mut options = MqttOptions::new(client_id, &printer.host, 8883);
        options.set_credentials("bblp", &printer.access_code);
        options.set_keep_alive(Duration::from_secs(15));
        options.set_max_packet_size(4 << 20, 1 << 20);
        options
            .set_transport(Transport::tls_with_config(TlsConfiguration::Rustls(tls::printer_config(&printer.serial)?)));
        let (client, events) = AsyncClient::new(options, 64);
        let (sender, reports) = watch::channel(Reports::default());
        let link = Arc::new(Self {
            client,
            request_topic: format!("device/{}/request", printer.serial),
            reports,
            pending: Mutex::new(HashMap::new()),
            sequence: AtomicU64::new(FIRST_SEQUENCE),
        });
        tokio::spawn(Arc::clone(&link).listen(events, format!("device/{}/report", printer.serial), sender));
        Ok(link)
    }

    /// The printer's state, as it changes.
    pub fn reports(&self) -> watch::Receiver<Reports> {
        self.reports.clone()
    }

    /// Send `body` as a request under `section`, such as `print`, and return
    /// the printer's answer.
    pub async fn request(&self, section: &str, mut body: Map<String, Value>) -> anyhow::Result<Value> {
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
        body.insert("sequence_id".into(), Value::String(sequence.to_string()));
        let (answer, answered) = oneshot::channel();
        self.lock_pending().insert(sequence, answer);
        let payload = serde_json::to_vec(&json!({ section: body }))?;
        let asked = async {
            self.client.publish(&self.request_topic, QoS::AtMostOnce, false, payload).await?;
            anyhow::Ok(answered.await?)
        };
        let answer = tokio::time::timeout(ANSWER_WITHIN, asked).await;
        self.lock_pending().remove(&sequence);
        answer.context("the printer did not answer")?
    }

    fn lock_pending(&self) -> std::sync::MutexGuard<'_, HashMap<u64, oneshot::Sender<Value>>> {
        self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    async fn listen(self: Arc<Self>, mut events: EventLoop, report_topic: String, reports: watch::Sender<Reports>) {
        let mut state = Value::Object(Map::new());
        let mut resync = tokio::time::interval(RESYNC_EVERY);
        loop {
            let event = tokio::select! {
                event = events.poll() => event,
                _ = resync.tick() => {
                    self.ask_everything();
                    continue;
                }
            };
            match event {
                Ok(Event::Incoming(Packet::ConnAck(_))) => {
                    tracing::info!("connected to the printer");
                    if let Err(error) = self.client.try_subscribe(&report_topic, QoS::AtMostOnce) {
                        tracing::warn!("cannot subscribe to the reports: {error}");
                    }
                    self.ask_everything();
                    reports.send_modify(|reports| reports.online = true);
                }
                Ok(Event::Incoming(Packet::Publish(publish))) => {
                    let Ok(message) = serde_json::from_slice::<Value>(&publish.payload) else {
                        tracing::warn!("the printer sent a report that is not JSON");
                        continue;
                    };
                    self.answer(&message);
                    if is_status(&message) {
                        merge(&mut state, message);
                        reports.send_replace(Reports {
                            online: true,
                            received_at: crate::now(),
                            state: Arc::new(state.clone()),
                        });
                    }
                }
                Ok(_) => {}
                Err(error) => {
                    if reports.borrow().online {
                        tracing::warn!("lost the printer: {error}");
                    }
                    reports.send_if_modified(|reports| std::mem::replace(&mut reports.online, false));
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
            }
        }
    }

    /// Ask for the whole state, which the printer sends as one report.
    fn ask_everything(&self) {
        let request = json!({ "pushing": { "sequence_id": "0", "command": "pushall" } });
        if let Err(error) = self.client.try_publish(&self.request_topic, QoS::AtMostOnce, false, request.to_string()) {
            tracing::warn!("cannot ask for the printer's state: {error}");
        }
    }

    /// Hand `message` to the request it answers, if any.
    fn answer(&self, message: &Value) {
        let Some(sections) = message.as_object() else { return };
        for body in sections.values() {
            let sequence = body.get("sequence_id").and_then(|id| match id {
                Value::String(text) => text.parse::<u64>().ok(),
                other => other.as_u64(),
            });
            if let Some(sequence) = sequence.filter(|&sequence| sequence >= FIRST_SEQUENCE)
                && let Some(answer) = self.lock_pending().remove(&sequence)
            {
                let _ = answer.send(body.clone());
            }
        }
    }
}

/// Whether `message` is a status report rather than an answer.
fn is_status(message: &Value) -> bool {
    message.pointer("/print/command").and_then(Value::as_str) == Some("push_status")
}

/// Merge `update` into `state`: objects field by field, anything else
/// replaced whole.
fn merge(state: &mut Value, update: Value) {
    match (state, update) {
        (Value::Object(state), Value::Object(update)) => {
            for (key, value) in update {
                match state.get_mut(&key) {
                    Some(slot) => merge(slot, value),
                    None => {
                        state.insert(key, value);
                    }
                }
            }
        }
        (state, update) => *state = update,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_merge_into_the_whole() {
        let mut state = json!({ "print": { "command": "push_status", "mc_percent": 82, "ams": { "tray_now": "1" } } });
        merge(&mut state, json!({ "print": { "command": "push_status", "mc_percent": 83, "hms": [] } }));
        assert_eq!(
            state,
            json!({ "print": { "command": "push_status", "mc_percent": 83, "hms": [], "ams": { "tray_now": "1" } } })
        );
        merge(&mut state, json!({ "print": { "hms": [{ "attr": 1, "code": 2 }] } }));
        assert_eq!(state["print"]["hms"], json!([{ "attr": 1, "code": 2 }]));
    }

    #[test]
    fn only_push_status_is_a_report() {
        assert!(is_status(&json!({ "print": { "command": "push_status" } })));
        assert!(!is_status(&json!({ "print": { "command": "pause", "result": "success" } })));
        assert!(!is_status(&json!({ "system": { "command": "ledctrl" } })));
    }
}
