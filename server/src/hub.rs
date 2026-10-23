//! The running server's parts, and the tasks that tie them: the printer's
//! reports made into the [`Printer`] pages see, the history sampled from it,
//! the plate's picture fetched for it, and the notifications sent of it.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::{broadcast, watch};
use x2d_protocol::{Printer, Sample};

use crate::camera::Camera;
use crate::commands::Log;
use crate::config::Config;
use crate::history::{self, History};
use crate::mqtt::{Link, Reports};
use crate::people::People;
use crate::report;
use crate::telegram::{self, Telegram};
use crate::texts::Texts;
use crate::thumbnail::Thumbnails;

/// Everything the web server reaches.
pub struct Hub {
    /// The configuration.
    pub config: Config,
    /// The link to the printer.
    pub link: Arc<Link>,
    /// The camera.
    pub camera: Camera,
    /// The people invited.
    pub people: People,
    /// The pictures of the plates printed.
    pub thumbnails: Arc<Thumbnails>,
    /// The log of commands.
    pub log: Log,
    /// The printer as pages see it.
    pub printer: watch::Receiver<Arc<Printer>>,
    /// Each new point of history.
    pub samples: broadcast::Sender<Sample>,
    history: Mutex<History>,
}

/// A printer seen less recently than this is not sampled.
const STALE_AFTER: i64 = 30;

impl Hub {
    /// Start every part, and the tasks between them.
    pub fn start(config: Config) -> anyhow::Result<Arc<Self>> {
        std::fs::create_dir_all(&config.data)?;
        let link = Link::start(&config.printer)?;
        let camera = Camera::new(&config.printer);
        let texts = Texts::open(&config.data, &config.printer.serial);
        let thumbnails = Thumbnails::new(&config.printer);
        let (sender, printer) = watch::channel(Arc::new(Printer::default()));
        tokio::spawn(view(link.reports(), Arc::clone(&texts), Arc::clone(&thumbnails), sender));
        tokio::spawn(Arc::clone(&thumbnails).follow(printer.clone()));
        if let Some(telegram) = &config.telegram {
            let path = config.data.join("notified.json");
            tokio::spawn(telegram::notify(
                Telegram::new(telegram),
                printer.clone(),
                camera.clone(),
                path,
                config.origin.clone(),
            ));
        }
        let hub = Arc::new(Self {
            history: Mutex::new(History::open(&config.data)?),
            people: People::new(&config.data),
            log: Log::new(&config.data),
            samples: broadcast::channel(16).0,
            config,
            link,
            camera,
            thumbnails,
            printer,
        });
        tokio::spawn(Arc::clone(&hub).sample());
        Ok(hub)
    }

    /// The history of the last day, as JSON.
    pub fn history_json(&self) -> anyhow::Result<Vec<u8>> {
        self.lock_history().json()
    }

    fn lock_history(&self) -> MutexGuard<'_, History> {
        self.history.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Take a sample of the temperatures every few seconds while the printer
    /// is heard.
    async fn sample(self: Arc<Self>) {
        let mut every = tokio::time::interval(Duration::from_secs(history::EVERY));
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            every.tick().await;
            let printer = Arc::clone(&self.printer.borrow());
            let now = crate::now();
            if !printer.online || now - printer.reported_at > STALE_AFTER {
                continue;
            }
            let sample = printer.sample(now);
            if let Err(error) = self.lock_history().push(sample.clone()) {
                tracing::warn!("cannot keep the history: {error:#}");
            }
            let _ = self.samples.send(sample);
        }
    }
}

/// Make the printer that pages see out of its reports, its alerts' texts
/// and its plate's picture, whenever one of them changes.
async fn view(
    mut reports: watch::Receiver<Reports>,
    texts: Arc<Texts>,
    thumbnails: Arc<Thumbnails>,
    printer: watch::Sender<Arc<Printer>>,
) {
    let mut learned = texts.learned();
    let mut fetched = thumbnails.fetched();
    loop {
        let mut seen = report::printer(&reports.borrow_and_update());
        texts.fill(&mut seen);
        thumbnails.fill(&mut seen);
        printer.send_if_modified(|current| {
            let changed = **current != seen;
            if changed {
                *current = Arc::new(seen);
            }
            changed
        });
        tokio::select! {
            changed = reports.changed() => if changed.is_err() { return },
            _ = learned.changed() => {}
            _ = fetched.changed() => {}
        }
    }
}
