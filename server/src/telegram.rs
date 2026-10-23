//! The Telegram Bot API, as far as x2d uses it: messages and pictures to the
//! chat of the configuration, the messages it receives, and the commands it
//! offers.
//!
//! Requests carry the bot's token in their URL: errors are told without it.

use std::time::Duration;

use serde_json::{Value, json};

use crate::config::TelegramConfig;

/// A bot and the chat it belongs to.
pub struct Telegram {
    http: reqwest::Client,
    api: String,
    chat: i64,
}

/// How long a request for messages waits for one to come.
const POLL_FOR: Duration = Duration::from_secs(25);

impl Telegram {
    /// The bot of `config`.
    pub fn new(config: &TelegramConfig) -> Self {
        Self {
            http: reqwest::Client::builder().timeout(POLL_FOR + Duration::from_secs(15)).build().unwrap_or_default(),
            api: format!("https://api.telegram.org/bot{}", config.token),
            chat: config.chat,
        }
    }

    /// The chat the bot belongs to: the only one it writes to and obeys.
    pub fn chat(&self) -> i64 {
        self.chat
    }

    /// Write `text` to the chat, without previews of its links.
    pub async fn text(&self, text: &str) -> anyhow::Result<()> {
        let body = json!({ "chat_id": self.chat, "text": text, "link_preview_options": { "is_disabled": true } });
        self.call("sendMessage", &body).await.map(drop)
    }

    /// Send the JPEG picture `jpeg` to the chat, with `caption`.
    pub async fn photo(&self, jpeg: Vec<u8>, caption: &str) -> anyhow::Result<()> {
        let form = reqwest::multipart::Form::new()
            .text("chat_id", self.chat.to_string())
            .text("caption", caption.to_owned())
            .part("photo", reqwest::multipart::Part::bytes(jpeg).file_name("x2d.jpg").mime_str("image/jpeg")?);
        answer(self.http.post(format!("{}/sendPhoto", self.api)).multipart(form).send().await).await.map(drop)
    }

    /// The messages that came after update `offset`, waiting a while for
    /// one if none did; asking from an offset confirms those before it.
    pub async fn updates(&self, offset: i64) -> anyhow::Result<Vec<Value>> {
        let body = json!({ "offset": offset, "timeout": POLL_FOR.as_secs(), "allowed_updates": ["message"] });
        let result = self.call("getUpdates", &body).await?;
        Ok(result.as_array().cloned().unwrap_or_default())
    }

    /// Offer `commands`, each a name and what it does, in the chat only.
    pub async fn offer(&self, commands: &[(&str, &str)]) -> anyhow::Result<()> {
        let commands: Vec<Value> = commands
            .iter()
            .map(|(command, description)| json!({ "command": command, "description": description }))
            .collect();
        let body = json!({ "commands": commands, "scope": { "type": "chat", "chat_id": self.chat } });
        self.call("setMyCommands", &body).await.map(drop)
    }

    async fn call(&self, method: &str, body: &Value) -> anyhow::Result<Value> {
        answer(self.http.post(format!("{}/{method}", self.api)).json(body).send().await).await
    }
}

/// The result of a call, or why it failed.
async fn answer(response: reqwest::Result<reqwest::Response>) -> anyhow::Result<Value> {
    let response = response.map_err(|error| anyhow::anyhow!("Telegram cannot be reached: {}", error.without_url()))?;
    let mut answer: Value =
        response.json().await.map_err(|error| anyhow::anyhow!("Telegram answered no JSON: {}", error.without_url()))?;
    if answer.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(answer.get_mut("result").map(Value::take).unwrap_or_default())
    } else {
        Err(anyhow::anyhow!("Telegram refused: {}", answer.get("description").and_then(Value::as_str).unwrap_or("?")))
    }
}
