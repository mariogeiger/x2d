//! A file fetched from the printer's storage by FTPS (RFC 959 over implicit
//! TLS on port 990, RFC 4217), as far as reading one file needs.
//!
//! The printer's vsftpd demands that the data connection resume the TLS
//! session of the control connection: both use the same configuration and
//! name, so that rustls resumes it.

use std::time::Duration;

use anyhow::{Context, bail};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use crate::config::PrinterConfig;
use crate::tls;

const PATIENCE: Duration = Duration::from_secs(20);

/// The file at `path` on the printer, if it is at most `limit` bytes long.
pub async fn fetch(printer: &PrinterConfig, path: &str, limit: usize) -> anyhow::Result<Vec<u8>> {
    tokio::time::timeout(PATIENCE * 3, fetch_within(printer, path, limit))
        .await
        .context("the transfer took too long")?
}

async fn fetch_within(printer: &PrinterConfig, path: &str, limit: usize) -> anyhow::Result<Vec<u8>> {
    let connector = TlsConnector::from(tls::printer_config(&printer.serial)?);
    let name = tls::printer_name(&printer.serial)?;
    let tcp = timeout(TcpStream::connect((printer.host.as_str(), 990))).await??;
    let mut control = Control { stream: BufReader::new(connector.connect(name.clone(), tcp).await?) };
    control.expect(None, 220).await?;
    control.expect(Some("USER bblp"), 331).await?;
    control.expect(Some(&format!("PASS {}", printer.access_code)), 230).await.context("the access code was refused")?;
    control.expect(Some("PBSZ 0"), 200).await?;
    control.expect(Some("PROT P"), 200).await?;
    control.expect(Some("TYPE I"), 200).await?;
    let passive = control.expect(Some("PASV"), 227).await?;
    let port = passive_port(&passive).context("the printer offered no passive port")?;
    let tcp = timeout(TcpStream::connect((printer.host.as_str(), port))).await??;
    let reply = control.ask(Some(&format!("RETR {path}"))).await?;
    if !matches!(reply.0, 125 | 150) {
        bail!("the printer has no {path}: {}", reply.1);
    }
    let mut data = connector.connect(name, tcp).await?;
    let mut bytes = Vec::new();
    let read = (&mut data).take(limit as u64 + 1).read_to_end(&mut bytes).await;
    if let Err(error) = read
        && error.kind() != std::io::ErrorKind::UnexpectedEof
    {
        return Err(error.into());
    }
    anyhow::ensure!(bytes.len() <= limit, "{path} is larger than {limit} bytes");
    drop(data);
    control.expect(None, 226).await.context("the transfer did not complete")?;
    let _ = control.ask(Some("QUIT")).await;
    Ok(bytes)
}

async fn timeout<T>(future: impl Future<Output = T>) -> anyhow::Result<T> {
    tokio::time::timeout(PATIENCE, future).await.context("the printer did not answer in time")
}

/// The control connection.
struct Control {
    stream: BufReader<TlsStream<TcpStream>>,
}

impl Control {
    /// Send `command`, if any, and return the reply, its code and text.
    async fn ask(&mut self, command: Option<&str>) -> anyhow::Result<(u16, String)> {
        if let Some(command) = command {
            self.stream.get_mut().write_all(format!("{command}\r\n").as_bytes()).await?;
        }
        let mut text = String::new();
        loop {
            let mut line = String::new();
            if timeout(self.stream.read_line(&mut line)).await?? == 0 {
                bail!("the printer closed the connection");
            }
            text.push_str(&line);
            // A reply ends with a line of its code and a space (RFC 959 §4.2).
            let first = text.get(..3).context("the printer sent a reply without a code")?;
            if line.len() >= 4 && line.starts_with(first) && line.as_bytes()[3] == b' ' {
                return Ok((first.parse().context("the printer sent a reply without a code")?, text));
            }
        }
    }

    /// Send `command`, if any, and return the reply's text, if its code is
    /// `code`.
    async fn expect(&mut self, command: Option<&str>, code: u16) -> anyhow::Result<String> {
        let (got, text) = self.ask(command).await?;
        if got != code {
            bail!("the printer answered {}", text.trim());
        }
        Ok(text)
    }
}

/// The port of a `227 Entering Passive Mode (h1,h2,h3,h4,p1,p2)` reply.
fn passive_port(reply: &str) -> Option<u16> {
    let inside = reply.split_once('(')?.1.split_once(')')?.0;
    let numbers: Vec<u16> = inside.split(',').map(|number| number.trim().parse().ok()).collect::<Option<_>>()?;
    let [_, _, _, _, high, low] = numbers[..] else { return None };
    Some(high * 256 + low)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_passive_port_reads() {
        assert_eq!(passive_port("227 Entering Passive Mode (192,168,0,95,195,81).\r\n"), Some(195 * 256 + 81));
        assert_eq!(passive_port("227 nothing"), None);
    }
}
