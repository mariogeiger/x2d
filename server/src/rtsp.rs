//! An RTSP client over TLS for the printer's camera (RFC 2326), as far as a
//! live H.264 stream needs: DESCRIBE, SETUP over the TCP connection itself
//! (interleaved), PLAY, and GET_PARAMETER to keep the session alive.
//!
//! The camera is a LIVE555 server that asks for digest authentication. Its
//! RTP packets and its answers share the connection, so the reader takes
//! whichever comes: a `$` and a channel opens a packet, anything else an
//! answer.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use base64::Engine;
use md5::{Digest, Md5};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_rustls::client::TlsStream;

use crate::config::PrinterConfig;
use crate::h264::Packet;
use crate::tls;

type Connection = TlsStream<TcpStream>;

/// A playing session: the camera's packets, in the order they come.
pub struct Session {
    reader: BufReader<ReadHalf<Connection>>,
    writer: Arc<Mutex<WriteHalf<Connection>>>,
    requests: Arc<Requests>,
    /// Packets that came while an answer was awaited.
    early: VecDeque<Packet>,
    /// The parameter sets the session description announced.
    pub parameter_sets: Vec<Vec<u8>>,
    payload_type: u8,
    keepalive: Option<JoinHandle<()>>,
}

/// What every request needs: its URL, its sequence, its credentials.
struct Requests {
    url: String,
    sequence: AtomicU32,
    user: String,
    password: String,
    challenge: std::sync::Mutex<Option<Challenge>>,
    session: std::sync::Mutex<Option<String>>,
}

/// How the camera asked to be authenticated.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Challenge {
    Basic,
    Digest { realm: String, nonce: String, qop: bool },
}

/// An answer of the camera.
#[derive(Debug)]
struct Answer {
    status: u16,
    headers: HashMap<String, String>,
    body: String,
}

/// How long the camera has to answer or to send a packet.
const PATIENCE: Duration = Duration::from_secs(10);
/// How often the session is kept alive, within LIVE555's 65 s.
const KEEPALIVE_EVERY: Duration = Duration::from_secs(25);

impl Session {
    /// Open the camera's live stream and start it playing.
    pub async fn open(printer: &PrinterConfig) -> anyhow::Result<Self> {
        let config = tls::printer_config(&printer.serial)?;
        let tcp = tokio::time::timeout(PATIENCE, TcpStream::connect((printer.host.as_str(), 322)))
            .await
            .context("the camera did not accept a connection in time")??;
        tcp.set_nodelay(true)?;
        let connection =
            tokio_rustls::TlsConnector::from(config).connect(tls::printer_name(&printer.serial)?, tcp).await?;
        let (reader, writer) = tokio::io::split(connection);
        let mut session = Self {
            reader: BufReader::with_capacity(1 << 16, reader),
            writer: Arc::new(Mutex::new(writer)),
            requests: Arc::new(Requests {
                url: format!("rtsps://{}:322/streaming/live/1", printer.host),
                sequence: AtomicU32::new(1),
                user: "bblp".into(),
                password: printer.access_code.clone(),
                challenge: std::sync::Mutex::new(None),
                session: std::sync::Mutex::new(None),
            }),
            early: VecDeque::new(),
            parameter_sets: Vec::new(),
            payload_type: 96,
            keepalive: None,
        };
        let url = session.requests.url.clone();
        let described = session.ask("DESCRIBE", &url, &[("Accept", "application/sdp")]).await?;
        let base = described.headers.get("content-base").cloned().unwrap_or_else(|| format!("{url}/"));
        let video = Description::parse(&described.body).context("the camera described no H.264 video")?;
        session.parameter_sets = video.parameter_sets;
        session.payload_type = video.payload_type;
        let track = if video.control.contains("://") { video.control } else { format!("{base}{}", video.control) };
        let set_up = session.ask("SETUP", &track, &[("Transport", "RTP/AVP/TCP;unicast;interleaved=0-1")]).await?;
        let id = set_up.headers.get("session").context("the camera set up no session")?;
        let id = id.split(';').next().unwrap_or(id).trim().to_owned();
        *session.requests.session.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(id);
        session.ask("PLAY", &url, &[("Range", "npt=0.000-")]).await?;
        let (writer, requests) = (Arc::clone(&session.writer), Arc::clone(&session.requests));
        session.keepalive = Some(tokio::spawn(async move {
            let mut every = tokio::time::interval_at(tokio::time::Instant::now() + KEEPALIVE_EVERY, KEEPALIVE_EVERY);
            loop {
                every.tick().await;
                let request = requests.request("GET_PARAMETER", &requests.url, &[]);
                if writer.lock().await.write_all(request.as_bytes()).await.is_err() {
                    return;
                }
            }
        }));
        Ok(session)
    }

    /// The next RTP packet of the video.
    pub async fn next_packet(&mut self) -> anyhow::Result<Packet> {
        if let Some(packet) = self.early.pop_front() {
            return Ok(packet);
        }
        loop {
            match tokio::time::timeout(PATIENCE, self.read()).await.context("the camera fell silent")?? {
                Incoming::Packet(packet) => return Ok(packet),
                Incoming::Answer(answer) if answer.status != 200 => {
                    bail!("the camera answered {} to keeping the session alive", answer.status)
                }
                Incoming::Answer(_) | Incoming::Other => {}
            }
        }
    }

    /// Send a request, authenticating when asked to, and return its answer.
    async fn ask(&mut self, method: &str, url: &str, headers: &[(&str, &str)]) -> anyhow::Result<Answer> {
        for _ in 0..2 {
            let request = self.requests.request(method, url, headers);
            self.writer.lock().await.write_all(request.as_bytes()).await?;
            let answer = loop {
                match tokio::time::timeout(PATIENCE, self.read()).await.context("the camera did not answer")?? {
                    Incoming::Answer(answer) => break answer,
                    Incoming::Packet(packet) => self.early.push_back(packet),
                    Incoming::Other => {}
                }
            };
            match answer.status {
                200 => return Ok(answer),
                401 if self.requests.lock_challenge().is_none() => {
                    let offered = answer
                        .headers
                        .get("www-authenticate")
                        .context("the camera asks for no known authentication")?;
                    *self.requests.lock_challenge() = Some(Challenge::parse(offered)?);
                }
                401 => bail!("the camera refused the access code"),
                status => bail!("the camera answered {status} to {method}"),
            }
        }
        bail!("the camera kept asking for authentication")
    }

    /// Read what comes next: an interleaved packet or an answer.
    async fn read(&mut self) -> anyhow::Result<Incoming> {
        let first = *self.reader.fill_buf().await?.first().context("the camera closed the connection")?;
        if first == b'$' {
            let mut header = [0_u8; 4];
            self.reader.read_exact(&mut header).await?;
            let mut data = vec![0_u8; usize::from(u16::from_be_bytes([header[2], header[3]]))];
            self.reader.read_exact(&mut data).await?;
            return Ok(if header[1] == 0 {
                rtp(&data, self.payload_type).map_or(Incoming::Other, Incoming::Packet)
            } else {
                Incoming::Other
            });
        }
        let mut head = String::new();
        loop {
            let mut line = String::new();
            if self.reader.read_line(&mut line).await? == 0 {
                bail!("the camera closed the connection");
            }
            if line.trim().is_empty() {
                break;
            }
            head.push_str(&line);
            if head.len() > 1 << 16 {
                bail!("the camera sent an answer too long");
            }
        }
        let mut lines = head.lines();
        let status = lines
            .next()
            .and_then(|line| line.strip_prefix("RTSP/1.0 "))
            .and_then(|rest| rest.get(..3)?.parse().ok())
            .context("the camera sent something that is not RTSP")?;
        let headers: HashMap<String, String> = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
            .collect();
        let length: usize = headers.get("content-length").and_then(|length| length.parse().ok()).unwrap_or(0);
        anyhow::ensure!(length <= 1 << 16, "the camera sent a description too long");
        let mut body = vec![0_u8; length];
        self.reader.read_exact(&mut body).await?;
        Ok(Incoming::Answer(Answer { status, headers, body: String::from_utf8_lossy(&body).into_owned() }))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(keepalive) = self.keepalive.take() {
            keepalive.abort();
        }
    }
}

enum Incoming {
    Packet(Packet),
    Answer(Answer),
    Other,
}

impl Requests {
    fn lock_challenge(&self) -> std::sync::MutexGuard<'_, Option<Challenge>> {
        self.challenge.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The text of a request, with its sequence, session and credentials.
    fn request(&self, method: &str, url: &str, headers: &[(&str, &str)]) -> String {
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
        let mut request = format!("{method} {url} RTSP/1.0\r\nCSeq: {sequence}\r\nUser-Agent: x2d\r\n");
        for (name, value) in headers {
            request.push_str(&format!("{name}: {value}\r\n"));
        }
        if let Some(session) = &*self.session.lock().unwrap_or_else(std::sync::PoisonError::into_inner) {
            request.push_str(&format!("Session: {session}\r\n"));
        }
        if let Some(challenge) = &*self.lock_challenge() {
            let mut client_nonce = [0_u8; 8];
            let _ = getrandom::fill(&mut client_nonce);
            let client_nonce: String = client_nonce.iter().map(|byte| format!("{byte:02x}")).collect();
            let answer = challenge.answer(&self.user, &self.password, method, url, sequence, &client_nonce);
            request.push_str(&format!("Authorization: {answer}\r\n"));
        }
        request.push_str("\r\n");
        request
    }
}

impl Challenge {
    /// The challenge a `WWW-Authenticate` header makes.
    fn parse(header: &str) -> anyhow::Result<Self> {
        let (scheme, parameters) = header.split_once(' ').unwrap_or((header, ""));
        match scheme.to_ascii_lowercase().as_str() {
            "basic" => Ok(Self::Basic),
            "digest" => {
                let mut fields = HashMap::new();
                for field in parameters.split(',') {
                    if let Some((name, value)) = field.split_once('=') {
                        fields.insert(name.trim().to_ascii_lowercase(), value.trim().trim_matches('"').to_owned());
                    }
                }
                Ok(Self::Digest {
                    realm: fields.remove("realm").context("the digest challenge has no realm")?,
                    nonce: fields.remove("nonce").context("the digest challenge has no nonce")?,
                    qop: fields.get("qop").is_some_and(|qop| qop.split(',').any(|qop| qop.trim() == "auth")),
                })
            }
            other => Err(anyhow!("the camera asks for {other} authentication, which is not known")),
        }
    }

    /// The `Authorization` header answering this challenge for one request.
    fn answer(&self, user: &str, password: &str, method: &str, url: &str, count: u32, client_nonce: &str) -> String {
        match self {
            Self::Basic => {
                format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}")))
            }
            Self::Digest { realm, nonce, qop } => {
                let md5 = |text: String| {
                    Md5::digest(text.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect::<String>()
                };
                let known = md5(format!("{user}:{realm}:{password}"));
                let asked = md5(format!("{method}:{url}"));
                if *qop {
                    let count = format!("{count:08x}");
                    let response = md5(format!("{known}:{nonce}:{count}:{client_nonce}:auth:{asked}"));
                    format!(
                        "Digest username=\"{user}\", realm=\"{realm}\", nonce=\"{nonce}\", uri=\"{url}\", qop=auth, nc={count}, cnonce=\"{client_nonce}\", response=\"{response}\""
                    )
                } else {
                    let response = md5(format!("{known}:{nonce}:{asked}"));
                    format!(
                        "Digest username=\"{user}\", realm=\"{realm}\", nonce=\"{nonce}\", uri=\"{url}\", response=\"{response}\""
                    )
                }
            }
        }
    }
}

/// What the session description says of its H.264 video.
#[derive(Debug, PartialEq, Eq)]
struct Description {
    control: String,
    payload_type: u8,
    parameter_sets: Vec<Vec<u8>>,
}

impl Description {
    /// The first H.264 video of the SDP `text`.
    fn parse(text: &str) -> Option<Self> {
        let video = text.split("\nm=").skip(1).find(|media| media.starts_with("video"))?;
        let attribute = |name: &'static str| attributes(video, name);
        let payload_type = attribute("rtpmap:").find_map(|map| {
            let (kind, codec) = map.split_once(' ')?;
            codec.to_ascii_uppercase().starts_with("H264/").then(|| kind.parse().ok())?
        })?;
        let parameter_sets = attribute("fmtp:")
            .filter_map(|format| format.split_once(' ').map(|(_, parameters)| parameters.to_owned()))
            .flat_map(|parameters| parameters.split(';').map(str::trim).map(str::to_owned).collect::<Vec<_>>())
            .filter_map(|parameter| parameter.strip_prefix("sprop-parameter-sets=").map(str::to_owned))
            .flat_map(|sets| sets.split(',').map(str::to_owned).collect::<Vec<_>>())
            .filter_map(|set| base64::engine::general_purpose::STANDARD.decode(set).ok())
            .collect();
        Some(Self { control: attribute("control:").next()?.to_owned(), payload_type, parameter_sets })
    }
}

/// The values of the attributes `a=<name>…` of the SDP section `media`.
fn attributes<'a>(media: &'a str, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
    media.lines().filter_map(move |line| line.trim().strip_prefix("a=")?.strip_prefix(name))
}

/// The RTP packet in `data` (RFC 3550), if it is one of payload type `kind`.
fn rtp(data: &[u8], kind: u8) -> Option<Packet> {
    let (&first, &second) = (data.first()?, data.get(1)?);
    if first >> 6 != 2 || second & 0x7F != kind {
        return None;
    }
    let mut start = 12 + 4 * usize::from(first & 0x0F);
    if first & 0x10 != 0 {
        let words = u16::from_be_bytes([*data.get(start + 2)?, *data.get(start + 3)?]);
        start += 4 + 4 * usize::from(words);
    }
    let mut end = data.len();
    if first & 0x20 != 0 {
        end = end.checked_sub(usize::from(*data.last()?))?;
    }
    Some(Packet {
        marker: second & 0x80 != 0,
        sequence: u16::from_be_bytes([data[2], data[3]]),
        timestamp: u32::from_be_bytes(data.get(4..8)?.try_into().ok()?),
        payload: data.get(start..end)?.to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The session description the X2D's camera gave on 2026-10-23.
    const X2D_DESCRIPTION: &str = "v=0\r\no=- 1792616653775855 1 IN IP4 192.168.0.95\r\ns=rtsp stream server\r\nt=0 0\r\na=tool:LIVE555 Streaming Media v2023.03.30\r\na=type:broadcast\r\na=control:*\r\na=range:npt=now-\r\nm=video 0 RTP/AVP 96\r\nc=IN IP4 0.0.0.0\r\nb=AS:1000\r\na=rtpmap:96 H264/90000\r\na=fmtp:96 packetization-mode=1;profile-level-id=641029;sprop-parameter-sets=Z2QQKawbGqB4AiflhAAAAwAEAAADAPI8IBCo,aO48sA==\r\na=control:track1\r\n";

    #[test]
    fn the_x2d_description_reads() {
        let description = Description::parse(X2D_DESCRIPTION).unwrap();
        assert_eq!(description.control, "track1");
        assert_eq!(description.payload_type, 96);
        assert_eq!(description.parameter_sets.len(), 2);
        assert_eq!(description.parameter_sets[1], [0x68, 0xEE, 0x3C, 0xB0]);
    }

    #[test]
    fn digest_answers_as_rfc_2617_computes() {
        // The worked example of RFC 2617 §3.5, with qop and without it as in
        // RFC 2069, which LIVE555 asks for.
        let offered =
            r#"Digest realm="testrealm@host.com", qop="auth,auth-int", nonce="dcd98b7102dd2f0e8b11d0f600bfb0c093""#;
        let answer = Challenge::parse(offered).unwrap().answer(
            "Mufasa",
            "Circle Of Life",
            "GET",
            "/dir/index.html",
            1,
            "0a4f113b",
        );
        assert!(answer.contains(r#"response="6629fae49393a05397450978507c4ef1""#), "{answer}");
        let offered = r#"Digest realm="testrealm@host.com", nonce="dcd98b7102dd2f0e8b11d0f600bfb0c093""#;
        let answer =
            Challenge::parse(offered).unwrap().answer("Mufasa", "Circle Of Life", "GET", "/dir/index.html", 1, "");
        assert!(answer.contains(r#"response="670fd8c2df070c60b045671b8b24ff02""#), "{answer}");
        assert_eq!(Challenge::parse("Basic realm=\"x\"").unwrap(), Challenge::Basic);
    }

    #[test]
    fn rtp_headers_strip() {
        let mut data = vec![0x80 | 0x20, 0x80 | 96, 0x04, 0x96, 0, 0, 0x0B, 0xB8, 1, 2, 3, 4];
        data.extend_from_slice(&[0x65, 0xAA, 0, 2]);
        let packet = rtp(&data, 96).unwrap();
        assert!(packet.marker);
        assert_eq!((packet.sequence, packet.timestamp), (1174, 3000));
        assert_eq!(packet.payload, [0x65, 0xAA]);
        assert!(rtp(&data, 97).is_none());
    }
}
