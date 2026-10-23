//! `x2d check`: reach the printer and its camera once, and tell what they
//! say, to see that the configuration holds before serving.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Context;
use x2d_protocol::stage_label;

use crate::camera::{Camera, Event};
use crate::config::Config;
use crate::mqtt::Link;
use crate::{report, thumbnail};

/// Tell what the printer and its camera say, and write in `pictures`, if
/// given, what the camera sees and the picture of the plate printed.
pub async fn run(config: &Config, pictures: Option<&Path>) -> anyhow::Result<()> {
    let link = Link::start(&config.printer)?;
    let mut reports = link.reports();
    tokio::time::timeout(Duration::from_secs(20), reports.wait_for(|reports| reports.received_at > 0))
        .await
        .context("the printer sent no report in time")??;
    let printer = report::printer(&reports.borrow());
    println!("printer: {}", printer.state.label());
    if let Some(stage) = printer.stage {
        println!("stage: {stage} ({})", stage_label(stage).unwrap_or("?"));
    }
    if let Some(job) = &printer.job {
        println!(
            "job: {} at {} %, {} min left, layer {}/{}",
            job.name, job.percent, job.remaining_minutes, job.layer, job.total_layers
        );
        if let Some(directory) = pictures {
            let png = thumbnail::picture(&config.printer, &job.name, job.plate).await?;
            let path = directory.join("plate.png");
            std::fs::write(&path, &png)?;
            println!("plate: {} ({} bytes)", path.display(), png.len());
        }
    }
    for nozzle in &printer.nozzles {
        println!(
            "{}: {} / {} °C{}",
            nozzle.label(printer.nozzles.len()),
            nozzle.heater.current,
            nozzle.heater.target,
            if nozzle.active { ", active" } else { "" }
        );
    }
    if let Some(bed) = printer.bed {
        println!("bed: {} / {} °C", bed.current, bed.target);
    }
    for alert in &printer.alerts {
        println!("alert: {}", alert.code);
    }

    let camera = Camera::new(&config.printer);
    let mut viewer = camera.watch();
    let started = Instant::now();
    let (mut frames, mut keys, mut bytes) = (0, 0, 0);
    while started.elapsed() < Duration::from_secs(5) {
        match tokio::time::timeout(Duration::from_secs(15), viewer.next()).await.context("the camera fell silent")? {
            Some(Event::Stream(stream)) => {
                println!("video: {} {}×{}", stream.config.codec, stream.config.width, stream.config.height);
            }
            Some(Event::Frame(frame)) => {
                frames += 1;
                keys += usize::from(frame.key);
                bytes += frame.message.len();
            }
            Some(Event::Lost(reason)) => anyhow::bail!("the camera stopped: {reason}"),
            None => anyhow::bail!("the camera closed"),
        }
    }
    let seconds = started.elapsed().as_secs_f64();
    println!("video: {frames} frames ({keys} key) in {seconds:.1} s, {:.2} Mbit/s", bytes as f64 * 8.0 / seconds / 1e6);
    drop(viewer);
    if let Some(directory) = pictures {
        let jpeg = camera.picture().await?;
        let path = directory.join("camera.jpg");
        std::fs::write(&path, &jpeg)?;
        println!("picture: {} ({} bytes)", path.display(), jpeg.len());
    }
    Ok(())
}
