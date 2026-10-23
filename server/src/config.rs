//! The configuration file: where the printer is, its secrets, and where the
//! server keeps its data. It lives outside the repository, readable by its
//! owner only, since it holds the printer's access code.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;

/// What the server and its commands are configured with.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The address the web server listens on.
    pub listen: SocketAddr,
    /// The origin the page is served from, such as `https://x2d.geiger.ink`:
    /// invitation links start with it, and requests from any other are
    /// refused.
    pub origin: String,
    /// The directory of the server's data: people, history, logs, caches.
    pub data: PathBuf,
    /// The directory of the built page.
    pub site: PathBuf,
    /// The printer.
    pub printer: PrinterConfig,
    /// Where to notify, if anywhere.
    pub telegram: Option<TelegramConfig>,
}

/// The printer on the LAN.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrinterConfig {
    /// Its address.
    pub host: String,
    /// Its serial number, which its certificate names.
    pub serial: String,
    /// Its LAN access code.
    pub access_code: String,
}

/// A Telegram bot and the chat it notifies.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramConfig {
    /// The bot's token.
    pub token: String,
    /// The chat to notify.
    pub chat: i64,
}

impl Config {
    /// The configuration in the TOML file at `path`.
    pub fn read(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
        let config: Self =
            toml::from_str(&text).with_context(|| format!("{} is not a valid configuration", path.display()))?;
        anyhow::ensure!(!config.origin.ends_with('/'), "the origin ends without a slash, as in https://x2d.geiger.ink");
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_configuration_reads() {
        let config: Config = toml::from_str(
            r#"
            listen = "127.0.0.1:4011"
            origin = "https://x2d.example"
            data = "/srv/x2d"
            site = "/srv/x2d/site"
            [printer]
            host = "192.168.0.95"
            serial = "SERIAL"
            access_code = "CODE"
            [telegram]
            token = "1:abc"
            chat = 42
            "#,
        )
        .unwrap();
        assert_eq!(config.listen.port(), 4011);
        assert_eq!(config.telegram.unwrap().chat, 42);
    }
}
