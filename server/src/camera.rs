//! The camera, opened only while someone watches, and shared by all who do.
//!
//! The first [`Viewer`] opens the camera's RTSPS stream; every viewer then
//! receives the same frames, already framed as the page's binary messages.
//! A new viewer starts from the last key frame, kept with the frames since,
//! so its picture comes at once. When the last viewer leaves, the stream
//! closes a little later, sparing the printer's network and processor.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use anyhow::Context;
use bytes::Bytes;
use tokio::io::AsyncWriteExt;
use tokio::sync::broadcast;
use x2d_protocol::video::encode_chunk;

use crate::config::PrinterConfig;
use crate::h264::{Depacketizer, Output, Stream};
use crate::rtsp::Session;

/// What viewers receive.
#[derive(Clone, Debug)]
pub enum Event {
    /// The video starts, or starts over, with these parameters.
    Stream(Arc<Stream>),
    /// A frame, framed as the page's binary message.
    Frame(Frame),
    /// The camera stopped sending, for this reason.
    Lost(String),
}

/// A frame, as a page receives it.
#[derive(Clone, Debug)]
pub struct Frame {
    /// Whether it decodes on its own.
    pub key: bool,
    /// The binary message of [`x2d_protocol::video::encode_chunk`].
    pub message: Bytes,
}

/// The camera of the printer.
#[derive(Clone)]
pub struct Camera {
    inner: Arc<Inner>,
}

struct Inner {
    printer: PrinterConfig,
    shared: Mutex<Shared>,
    events: broadcast::Sender<Event>,
}

#[derive(Default)]
struct Shared {
    viewers: usize,
    running: bool,
    stream: Option<Arc<Stream>>,
    /// The frames since the last key frame, the key frame first.
    group: Vec<Frame>,
}

/// One who watches: what came before they did, then what comes.
pub struct Viewer {
    /// The stream and the frames since its last key frame, at the time the
    /// viewer came.
    pub backlog: Vec<Event>,
    /// What comes after.
    pub events: broadcast::Receiver<Event>,
    inner: Arc<Inner>,
}

/// How long the stream stays open after its last viewer left.
const LINGER: Duration = Duration::from_secs(10);
/// How long to wait before opening the stream again after a failure.
const RETRY_AFTER: Duration = Duration::from_secs(3);
/// The most frames kept for newcomers: a few seconds' worth at 30 fps.
const GROUP_AT_MOST: usize = 300;
/// The frames a viewer may fall behind before it skips to a key frame.
const QUEUE: usize = 90;

impl Camera {
    /// The camera of `printer`, closed until someone watches.
    pub fn new(printer: &PrinterConfig) -> Self {
        let (events, _) = broadcast::channel(QUEUE);
        Self { inner: Arc::new(Inner { printer: printer.clone(), shared: Mutex::new(Shared::default()), events }) }
    }

    /// Start watching, which opens the stream unless it is open already.
    pub fn watch(&self) -> Viewer {
        let mut shared = self.inner.lock();
        shared.viewers += 1;
        if !shared.running {
            shared.running = true;
            tokio::spawn(Arc::clone(&self.inner).run());
        }
        let backlog = shared
            .stream
            .iter()
            .map(|stream| Event::Stream(Arc::clone(stream)))
            .chain(shared.group.iter().cloned().map(Event::Frame))
            .collect();
        Viewer { backlog, events: self.inner.events.subscribe(), inner: Arc::clone(&self.inner) }
    }

    /// A JPEG picture of what the camera sees now, decoded by `ffmpeg` from
    /// the next key frame.
    pub async fn picture(&self) -> anyhow::Result<Vec<u8>> {
        let mut viewer = self.watch();
        let (stream, frame) = tokio::time::timeout(Duration::from_secs(20), viewer.key_frame())
            .await
            .context("the camera sent no picture in time")??;
        let mut annex_b = Vec::new();
        for unit in [&stream.sps[..], &stream.pps[..]].into_iter().chain(units(&frame.message[9..])) {
            annex_b.extend_from_slice(&[0, 0, 0, 1]);
            annex_b.extend_from_slice(unit);
        }
        let mut ffmpeg = tokio::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "h264",
                "-i",
                "pipe:0",
                "-frames:v",
                "1",
                "-q:v",
                "3",
                "-f",
                "image2",
                "-c:v",
                "mjpeg",
                "pipe:1",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("ffmpeg, which decodes pictures, cannot run")?;
        let mut input = ffmpeg.stdin.take().context("ffmpeg has no input")?;
        input.write_all(&annex_b).await?;
        drop(input);
        let output = ffmpeg.wait_with_output().await?;
        anyhow::ensure!(output.status.success() && !output.stdout.is_empty(), "ffmpeg did not decode the picture");
        Ok(output.stdout)
    }
}

/// The NAL units of `data`, each behind its big-endian `u32` length.
fn units(mut data: &[u8]) -> impl Iterator<Item = &[u8]> {
    std::iter::from_fn(move || {
        let (length, rest) = data.split_first_chunk::<4>()?;
        let (unit, rest) = rest.split_at_checked(u32::from_be_bytes(*length) as usize)?;
        data = rest;
        Some(unit)
    })
}

impl Viewer {
    /// The next event for this viewer. When the viewer fell too far behind,
    /// what it missed is skipped and it resumes at the next key frame.
    pub async fn next(&mut self) -> Option<Event> {
        if !self.backlog.is_empty() {
            return Some(self.backlog.remove(0));
        }
        let mut skipping = false;
        loop {
            match self.events.recv().await {
                Ok(Event::Frame(frame)) if skipping && !frame.key => {}
                Ok(event) => return Some(event),
                Err(broadcast::error::RecvError::Lagged(_)) => skipping = true,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }

    /// The stream and the first key frame that comes.
    async fn key_frame(&mut self) -> anyhow::Result<(Arc<Stream>, Frame)> {
        let mut stream = None;
        loop {
            match self.next().await.context("the camera closed")? {
                Event::Stream(started) => stream = Some(started),
                Event::Frame(frame) if frame.key => {
                    if let Some(stream) = stream {
                        return Ok((stream, frame));
                    }
                }
                Event::Frame(_) => {}
                Event::Lost(reason) => anyhow::bail!("the camera stopped: {reason}"),
            }
        }
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        self.inner.lock().viewers -= 1;
    }
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Stream for as long as someone watches, opening the stream again
    /// after each failure.
    async fn run(self: Arc<Self>) {
        loop {
            let ended = self.stream().await;
            if let Err(error) = &ended {
                tracing::warn!("the camera stopped: {error:#}");
                let _ = self.events.send(Event::Lost(format!("{error:#}")));
            }
            {
                let mut shared = self.lock();
                shared.stream = None;
                shared.group.clear();
                if shared.viewers == 0 {
                    shared.running = false;
                    return;
                }
            }
            if ended.is_err() {
                tokio::time::sleep(RETRY_AFTER).await;
            }
        }
    }

    /// Open the stream and pass its frames on, until it fails or nobody has
    /// watched for a while.
    async fn stream(&self) -> anyhow::Result<()> {
        let mut session = Session::open(&self.printer).await?;
        tracing::info!("the camera streams");
        let mut depacketizer = Depacketizer::new(&session.parameter_sets);
        let mut clock = Clock::default();
        let mut alone_since: Option<Instant> = None;
        loop {
            let packet = session.next_packet().await?;
            for output in depacketizer.push(packet) {
                let mut shared = self.lock();
                match output {
                    Output::Stream(stream) => {
                        let stream = Arc::new(stream);
                        shared.stream = Some(Arc::clone(&stream));
                        shared.group.clear();
                        clock = Clock::default();
                        let _ = self.events.send(Event::Stream(stream));
                    }
                    Output::Frame(frame) => {
                        let message =
                            Bytes::from(encode_chunk(frame.key, clock.microseconds(frame.timestamp), &frame.data));
                        let frame = Frame { key: frame.key, message };
                        if frame.key {
                            shared.group.clear();
                        }
                        if shared.group.len() < GROUP_AT_MOST {
                            shared.group.push(frame.clone());
                        }
                        let _ = self.events.send(Event::Frame(frame));
                    }
                }
            }
            if self.lock().viewers > 0 {
                alone_since = None;
            } else if alone_since.get_or_insert_with(Instant::now).elapsed() > LINGER {
                tracing::info!("nobody watches: the camera closes");
                return Ok(());
            }
        }
    }
}

/// The 90 kHz RTP clock, unwrapped, in microseconds since the stream started.
#[derive(Default)]
struct Clock {
    first: Option<u32>,
    last: u32,
    ticks: i64,
}

impl Clock {
    fn microseconds(&mut self, timestamp: u32) -> u64 {
        if self.first.is_none() {
            self.first = Some(timestamp);
            self.last = timestamp;
        }
        self.ticks += i64::from(timestamp.wrapping_sub(self.last) as i32);
        self.last = timestamp;
        u64::try_from(self.ticks.max(0)).unwrap_or(0) * 1_000 / 90
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clock_unwraps() {
        let mut clock = Clock::default();
        assert_eq!(clock.microseconds(u32::MAX - 2_999), 0);
        assert_eq!(clock.microseconds(0), 33_333, "a frame later, past the wrap");
        assert_eq!(clock.microseconds(3_000), 66_666);
    }

    #[test]
    fn units_split() {
        let data = [0, 0, 0, 2, 0x65, 1, 0, 0, 0, 1, 0x41];
        assert_eq!(units(&data).collect::<Vec<_>>(), [&[0x65, 1][..], &[0x41][..]]);
    }
}
