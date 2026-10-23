//! The page's state: what the server said and what the reader chose, as
//! signals the view reads.

use leptos::prelude::*;
use serde::Deserialize;
use x2d_protocol::{Command, Printer, Role, Sample};

/// How far back the temperature history reaches, in seconds.
pub const HISTORY_SPAN: i64 = 24 * 3600;

/// Who reads the page, as the server knows them.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Reader {
    /// Their name.
    pub name: String,
    /// What they may do.
    pub role: Role,
}

/// Whether the reader may see the page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Access {
    /// The server is being asked.
    Checking,
    /// The reader is not signed in.
    Denied,
    /// The server did not answer.
    Unreachable,
    /// The reader is signed in.
    Granted(Reader),
}

/// Where the page's link to the server stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Connection {
    /// Opening.
    Connecting,
    /// Open.
    Open,
    /// Closed; the page tries again after this many seconds.
    Retrying { seconds: u32 },
}

/// What the video area shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VideoStatus {
    /// The reader turned the video off.
    Off,
    /// The tab is hidden, so nothing is sent.
    Hidden,
    /// Asked for, no picture yet.
    Starting,
    /// Pictures arrive and show.
    Playing,
    /// The camera or the decoder failed, for the reason given.
    Lost(String),
    /// This browser cannot decode the video.
    Unsupported,
}

/// What became of a command, shown for a few seconds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    /// The command's number.
    pub id: u32,
    /// What to say.
    pub text: String,
    /// Whether it went well.
    pub ok: bool,
}

/// Everything the view reads, as signals; copied freely.
#[derive(Clone, Copy)]
pub struct Page {
    /// Whether the reader may see the page, and who they are.
    pub access: RwSignal<Access>,
    /// Where the link to the server stands.
    pub connection: RwSignal<Connection>,
    /// The printer as last told; none before the first word of the server.
    pub printer: RwSignal<Option<Printer>>,
    /// The temperatures of the last 24 h, oldest first.
    pub history: RwSignal<Vec<Sample>>,
    /// What the video area shows.
    pub video: RwSignal<VideoStatus>,
    /// Whether the reader wants the video.
    pub watching: RwSignal<bool>,
    /// The commands sent and not answered yet, by number.
    pub pending: RwSignal<Vec<(u32, Command)>>,
    /// The outcomes of commands, newest last.
    pub notices: RwSignal<Vec<Notice>>,
    /// The command awaiting the reader's confirmation.
    pub confirming: RwSignal<Option<Command>>,
    /// Now, in milliseconds since the epoch, ticking every few seconds.
    pub clock: RwSignal<f64>,
}

impl Page {
    /// The state of a page that knows nothing yet.
    pub fn new() -> Self {
        Self {
            access: RwSignal::new(Access::Checking),
            connection: RwSignal::new(Connection::Connecting),
            printer: RwSignal::new(None),
            history: RwSignal::new(Vec::new()),
            video: RwSignal::new(VideoStatus::Starting),
            watching: RwSignal::new(true),
            pending: RwSignal::new(Vec::new()),
            notices: RwSignal::new(Vec::new()),
            confirming: RwSignal::new(None),
            clock: RwSignal::new(js_sys::Date::now()),
        }
    }

    /// The reader, once signed in.
    pub fn reader(&self) -> Option<Reader> {
        match self.access.get() {
            Access::Granted(reader) => Some(reader),
            _ => None,
        }
    }

    /// Adds a point to the history, and forgets what is older than a day.
    pub fn record(&self, sample: Sample) {
        self.history.update(|history| {
            if history.last().is_some_and(|last| last.time >= sample.time) {
                return;
            }
            let oldest = sample.time - HISTORY_SPAN;
            history.push(sample);
            let stale = history.partition_point(|kept| kept.time < oldest);
            history.drain(..stale);
        });
    }
}
