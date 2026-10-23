//! The picture of the plate being printed, which the slicer put in the
//! project file it sent: `Metadata/plate_<n>.png` of the `.3mf` archive on
//! the printer's storage, fetched by FTPS once per print.

use std::io::{Cursor, Read};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use anyhow::Context;
use bytes::Bytes;
use sha2::{Digest, Sha256};
use tokio::sync::watch;
use x2d_protocol::Printer;

use crate::config::PrinterConfig;
use crate::ftps;

/// The picture of the current print, once fetched.
pub struct Thumbnails {
    printer: PrinterConfig,
    current: Mutex<Option<Thumbnail>>,
    fetched: watch::Sender<u64>,
}

struct Thumbnail {
    /// The print it shows: its name and plate.
    job: (String, u32),
    /// Its name on the server: the start of its SHA-256 digest.
    key: String,
    png: Bytes,
}

/// The largest project file fetched.
const LARGEST: usize = 256 << 20;
/// How many times a project file is looked for, and how long apart.
const TRIES: u32 = 3;
const TRY_EVERY: Duration = Duration::from_secs(30);

impl Thumbnails {
    /// The pictures of `printer`'s prints, none fetched yet.
    pub fn new(printer: &PrinterConfig) -> Arc<Self> {
        Arc::new(Self { printer: printer.clone(), current: Mutex::new(None), fetched: watch::channel(0).0 })
    }

    /// Changes whenever a picture is fetched.
    pub fn fetched(&self) -> watch::Receiver<u64> {
        self.fetched.subscribe()
    }

    /// Give `printer`'s job the path of its picture, once fetched.
    pub fn fill(&self, printer: &mut Printer) {
        let current = self.lock();
        if let (Some(job), Some(thumbnail)) = (&mut printer.job, &*current)
            && thumbnail.job == (job.name.clone(), job.plate)
        {
            job.thumbnail = Some(format!("/api/thumbnail/{}", thumbnail.key));
        }
    }

    /// The picture named `key`, if it is the current one.
    pub fn get(&self, key: &str) -> Option<Bytes> {
        self.lock().as_ref().filter(|thumbnail| thumbnail.key == key).map(|thumbnail| thumbnail.png.clone())
    }

    fn lock(&self) -> MutexGuard<'_, Option<Thumbnail>> {
        self.current.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Fetch the picture of each print the printer starts, for as long as
    /// the server runs.
    pub async fn follow(self: Arc<Self>, mut printer: watch::Receiver<Arc<Printer>>) {
        let mut sought = None;
        loop {
            let job = printer.borrow_and_update().job.as_ref().map(|job| (job.name.clone(), job.plate));
            if let Some(job) = job
                && sought.as_ref() != Some(&job)
            {
                sought = Some(job.clone());
                self.seek(job).await;
            }
            if printer.changed().await.is_err() {
                return;
            }
        }
    }

    async fn seek(&self, job: (String, u32)) {
        for attempt in 1..=TRIES {
            match picture(&self.printer, &job.0, job.1).await {
                Ok(png) => {
                    let key: String = Sha256::digest(&png)[..8].iter().map(|byte| format!("{byte:02x}")).collect();
                    tracing::info!("fetched the picture of {}", job.0);
                    *self.lock() = Some(Thumbnail { job, key, png: Bytes::from(png) });
                    self.fetched.send_modify(|count| *count += 1);
                    return;
                }
                Err(error) => tracing::info!("no picture of {} yet ({attempt}/{TRIES}): {error:#}", job.0),
            }
            tokio::time::sleep(TRY_EVERY).await;
        }
    }
}

/// The picture of plate `plate` in the project file of the print `name`, on
/// `printer`'s storage.
pub async fn picture(printer: &PrinterConfig, name: &str, plate: u32) -> anyhow::Result<Vec<u8>> {
    let mut last = None;
    for path in [
        format!("/{name}.gcode.3mf"),
        format!("/cache/{name}.gcode.3mf"),
        format!("/{name}.3mf"),
        format!("/cache/{name}.3mf"),
    ] {
        match ftps::fetch(printer, &path, LARGEST).await {
            Ok(project) => return plate_picture(project, plate),
            Err(error) => last = Some(error.context(path)),
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("no project file")))
}

/// The picture of plate `plate` in the 3MF archive `project`.
fn plate_picture(project: Vec<u8>, plate: u32) -> anyhow::Result<Vec<u8>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(project)).context("the project file is not a 3MF archive")?;
    let mut entry = archive
        .by_name(&format!("Metadata/plate_{plate}.png"))
        .context("the project file has no picture of the plate")?;
    let mut png = Vec::new();
    entry.read_to_end(&mut png)?;
    Ok(png)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn the_plate_picture_comes_out_of_the_archive() {
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        archive.start_file("Metadata/plate_2.png", zip::write::SimpleFileOptions::default()).unwrap();
        archive.write_all(b"\x89PNG").unwrap();
        let project = archive.finish().unwrap().into_inner();
        assert_eq!(plate_picture(project.clone(), 2).unwrap(), b"\x89PNG");
        assert!(plate_picture(project, 1).is_err());
    }
}
