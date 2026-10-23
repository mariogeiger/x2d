//! The temperature history: one [`Sample`] every few seconds over the last
//! day, kept in memory and on disk, so that a restart loses nothing.
//!
//! The file `history.jsonl` takes one sample per line as it comes, and is
//! rewritten without the samples older than a day once an hour.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::Context;
use x2d_protocol::Sample;

/// How far back the history goes, in seconds.
pub const SPAN: i64 = 24 * 3600;
/// How often a sample is taken, in seconds.
pub const EVERY: u64 = 5;
/// How often the file is rewritten without its old samples, in seconds.
const COMPACT_EVERY: i64 = 3600;

/// The samples of the last day.
pub struct History {
    path: PathBuf,
    file: File,
    samples: VecDeque<Sample>,
    compacted_at: i64,
}

impl History {
    /// The history kept in `data`, as it was left.
    pub fn open(data: &Path) -> anyhow::Result<Self> {
        let path = data.join("history.jsonl");
        let mut samples = VecDeque::new();
        if let Ok(file) = File::open(&path) {
            let since = crate::now() - SPAN;
            for line in BufReader::new(file).lines() {
                if let Ok(sample) = serde_json::from_str::<Sample>(&line?)
                    && sample.time >= since
                {
                    samples.push_back(sample);
                }
            }
        }
        let file = rewrite(&path, &samples)?;
        Ok(Self { path, file, samples, compacted_at: crate::now() })
    }

    /// Add `sample`, the newest.
    pub fn push(&mut self, sample: Sample) -> anyhow::Result<()> {
        let mut line = serde_json::to_vec(&sample)?;
        line.push(b'\n');
        self.file.write_all(&line)?;
        let since = sample.time - SPAN;
        self.samples.push_back(sample);
        while self.samples.front().is_some_and(|oldest| oldest.time < since) {
            self.samples.pop_front();
        }
        if self.samples.back().is_some_and(|newest| newest.time - self.compacted_at > COMPACT_EVERY) {
            self.file = rewrite(&self.path, &self.samples)?;
            self.compacted_at = crate::now();
        }
        Ok(())
    }

    /// The samples of the last day, oldest first, as JSON.
    pub fn json(&self) -> anyhow::Result<Vec<u8>> {
        Ok(serde_json::to_vec(&self.samples)?)
    }
}

/// Write `samples` whole to `path`, and return the file open for appending.
fn rewrite(path: &Path, samples: &VecDeque<Sample>) -> anyhow::Result<File> {
    let fresh = path.with_extension("jsonl.new");
    let mut text = Vec::new();
    for sample in samples {
        serde_json::to_writer(&mut text, sample)?;
        text.push(b'\n');
    }
    std::fs::write(&fresh, text)?;
    std::fs::rename(&fresh, path)?;
    OpenOptions::new().append(true).open(path).with_context(|| format!("cannot append to {}", path.display()))
}

#[cfg(test)]
mod tests {
    use x2d_protocol::Heater;

    use super::*;

    #[test]
    fn the_history_survives_a_restart_and_forgets_the_day_before() {
        let directory = std::env::temp_dir().join(format!("x2d-history-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let now = crate::now();
        let sample =
            |time| Sample { time, nozzles: vec![Heater { current: 220.0, target: 220.0 }], bed: None, chamber: None };
        {
            let mut history = History::open(&directory).unwrap();
            history.push(sample(now - SPAN - 10)).unwrap();
            history.push(sample(now - 10)).unwrap();
            history.push(sample(now - 5)).unwrap();
        }
        let history = History::open(&directory).unwrap();
        let samples: Vec<Sample> = serde_json::from_slice(&history.json().unwrap()).unwrap();
        assert_eq!(samples, [sample(now - 10), sample(now - 5)]);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
