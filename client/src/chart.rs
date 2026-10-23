//! The temperature chart: the last hour, six hours or day of every heater,
//! drawn as SVG at the width of the page.
//!
//! A heater draws two lines: where it was, solid, and where it headed,
//! dashed, left out while it was off. Points closer than a pixel are drawn
//! as one, the latest; a line breaks where the server heard nothing.

use leptos::prelude::*;
use x2d_protocol::{Heater, Nozzle, Sample};

use crate::format::{clock_time, zone_offset};
use crate::state::Page;

/// The chart's height, in pixels.
const HEIGHT: f64 = 200.0;
/// Room for the temperatures on the left.
const LEFT: f64 = 36.0;
/// Room on the right.
const RIGHT: f64 = 10.0;
/// Room above.
const TOP: f64 = 8.0;
/// Room for the times below.
const BOTTOM: f64 = 22.0;
/// The longest silence a line spans without breaking, in seconds.
const GAP: i64 = 30;

/// A span of time the reader can choose, in seconds, with its name and the
/// step between its time ticks.
const SPANS: [(i64, &str, i64); 3] = [(3600, "1 h", 600), (6 * 3600, "6 h", 3600), (24 * 3600, "24 h", 4 * 3600)];

/// A heater of the history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Series {
    /// The nozzle of this id.
    Nozzle(usize),
    /// The bed.
    Bed,
    /// The chamber.
    Chamber,
}

impl Series {
    fn of(self, sample: &Sample) -> Option<Heater> {
        match self {
            Self::Nozzle(id) => sample.nozzles.get(id).copied(),
            Self::Bed => sample.bed,
            Self::Chamber => sample.chamber,
        }
    }

    /// Its CSS class, which gives its colour.
    pub fn class(self) -> String {
        match self {
            Self::Nozzle(id) => format!("nozzle{id}"),
            Self::Bed => "bed".into(),
            Self::Chamber => "chamber".into(),
        }
    }

    /// Its name, among `nozzles` nozzles.
    pub fn label(self, nozzles: usize) -> String {
        match self {
            Self::Nozzle(id) => Nozzle { id: id as u8, ..Nozzle::default() }.label(nozzles),
            Self::Bed => "Plateau".into(),
            Self::Chamber => "Chambre".into(),
        }
    }
}

/// Where the chart draws: the times it spans, the top of its scale, and its
/// width in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    /// The first second shown.
    pub from: i64,
    /// The last second shown.
    pub to: i64,
    /// The temperature at the top.
    pub top: f32,
    /// The width, in pixels.
    pub width: f64,
}

impl Frame {
    fn x(&self, time: i64) -> f64 {
        LEFT + (time - self.from) as f64 / (self.to - self.from).max(1) as f64 * (self.width - LEFT - RIGHT)
    }

    fn y(&self, value: f32) -> f64 {
        TOP + (1.0 - f64::from(value / self.top)) * (HEIGHT - TOP - BOTTOM)
    }

    /// How many seconds a pixel spans.
    fn per_pixel(&self) -> f64 {
        (self.to - self.from) as f64 / (self.width - LEFT - RIGHT).max(1.0)
    }
}

/// The heaters `samples` record, in the order of the legend.
pub fn series_in(samples: &[Sample]) -> Vec<Series> {
    let nozzles = samples.iter().map(|sample| sample.nozzles.len()).max().unwrap_or(0);
    let mut series: Vec<Series> = (0..nozzles).map(Series::Nozzle).collect();
    if samples.iter().any(|sample| sample.bed.is_some()) {
        series.push(Series::Bed);
    }
    if samples.iter().any(|sample| sample.chamber.is_some()) {
        series.push(Series::Chamber);
    }
    series
}

/// The top of the scale for `samples`: the hottest reading or target, with
/// room, on a multiple of 50 °C.
pub fn scale_top(samples: &[Sample]) -> f32 {
    let hottest = samples
        .iter()
        .flat_map(|sample| sample.nozzles.iter().copied().chain(sample.bed).chain(sample.chamber))
        .map(|heater| heater.current.max(heater.target))
        .fold(0.0_f32, f32::max);
    (((hottest + 5.0) / 50.0).ceil() * 50.0).max(50.0)
}

/// The SVG path of one line: `series`'s readings, or its targets, over
/// `samples` in `frame`.
pub fn path(samples: &[Sample], series: Series, target: bool, frame: &Frame) -> String {
    let mut path = String::new();
    let mut last: Option<(i64, i64)> = None;
    let mut pending: Option<(i64, f32)> = None;
    let flush = |path: &mut String, point: Option<(i64, f32)>, start: bool| {
        if let Some((time, value)) = point {
            let command = if start { 'M' } else { 'L' };
            path.push_str(&format!("{command}{:.1} {:.1}", frame.x(time), frame.y(value)));
        }
    };
    let mut start = true;
    for sample in samples.iter().filter(|sample| (frame.from..=frame.to).contains(&sample.time)) {
        let value = series.of(sample).map(|heater| if target { heater.target } else { heater.current });
        let value = value.filter(|value| !target || *value > 0.0);
        let bucket = ((sample.time - frame.from) as f64 / frame.per_pixel()) as i64;
        let broken = last.is_none_or(|(time, _)| sample.time - time > GAP.max(2 * frame.per_pixel() as i64));
        match value {
            None => {
                flush(&mut path, pending.take(), start);
                start = true;
                last = None;
                continue;
            }
            Some(value) => {
                if broken {
                    flush(&mut path, pending.take(), start);
                    start = true;
                } else if last.is_some_and(|(_, previous)| previous != bucket) {
                    flush(&mut path, pending.take(), start);
                    start = false;
                }
                pending = Some((sample.time, value));
                last = Some((sample.time, bucket));
            }
        }
    }
    flush(&mut path, pending, start);
    path
}

/// The temperatures marked on the scale up to `top`.
pub fn degree_ticks(top: f32) -> Vec<f32> {
    let step = if top <= 100.0 {
        25.0
    } else if top <= 300.0 {
        50.0
    } else {
        100.0
    };
    (0..).map(|i| i as f32 * step).take_while(|value| *value <= top).collect()
}

/// The seconds marked on the time axis between `from` and `to`: every
/// `step` of local time, for a zone `offset` seconds behind UTC.
pub fn time_ticks(from: i64, to: i64, step: i64, offset: i64) -> Vec<i64> {
    let first = (from - offset).div_euclid(step) * step + offset;
    (0..).map(|i| first + i * step).filter(|time| *time >= from).take_while(|time| *time <= to).collect()
}

/// The chart, with its span selector and legend.
#[component]
pub fn TemperatureChart() -> impl IntoView {
    let page = expect_context::<Page>();
    let span = RwSignal::new(0_usize);
    let width = RwSignal::new(600.0_f64);
    let holder = NodeRef::<leptos::html::Div>::new();
    let measure = move || {
        if let Some(element) = holder.get_untracked() {
            let measured = f64::from(element.client_width());
            if measured > 0.0 {
                width.set(measured);
            }
        }
    };
    Effect::new(move |_| {
        if holder.get().is_some() {
            measure();
        }
    });
    let resized = window_event_listener(leptos::ev::resize, move |_| measure());
    on_cleanup(move || resized.remove());

    let drawing = Memo::new(move |_| {
        let (seconds, _, step) = SPANS[span.get()];
        let now = (page.clock.get() / 1000.0) as i64;
        page.history.with(|history| {
            let to = history.last().map_or(now, |last| last.time.max(now));
            let from = to - seconds;
            let start = history.partition_point(|sample| sample.time < from);
            let visible = &history[start..];
            let frame = Frame { from, to, top: scale_top(visible), width: width.get() };
            let series = series_in(visible);
            let nozzles = series.iter().filter(|series| matches!(series, Series::Nozzle(_))).count();
            let lines: Vec<(String, String, bool)> = series
                .iter()
                .flat_map(|series| {
                    [false, true].map(|target| (series.class(), path(visible, *series, target, &frame), target))
                })
                .filter(|(_, path, _)| !path.is_empty())
                .collect();
            let legend: Vec<(String, String)> =
                series.iter().map(|series| (series.class(), series.label(nozzles))).collect();
            let offset = zone_offset(to as f64 * 1000.0);
            let times: Vec<(f64, String)> = time_ticks(from, to, step, offset)
                .into_iter()
                .map(|time| (frame.x(time), clock_time(time as f64 * 1000.0)))
                .collect();
            let degrees: Vec<(f64, String)> =
                degree_ticks(frame.top).into_iter().map(|value| (frame.y(value), format!("{value}"))).collect();
            (frame, lines, legend, times, degrees)
        })
    });

    view! {
        <section class="card chart-card">
            <div class="card-head">
                <h2>"Températures"</h2>
                <div class="segmented">
                    {SPANS
                        .iter()
                        .enumerate()
                        .map(|(index, (_, name, _))| {
                            view! {
                                <button class:chosen=move || span.get() == index on:click=move |_| span.set(index)>
                                    {*name}
                                </button>
                            }
                        })
                        .collect_view()}
                </div>
            </div>
            <div class="chart" node_ref=holder>
                {move || {
                    let (frame, lines, _, times, degrees) = drawing.get();
                    let right = frame.width - RIGHT;
                    let bottom = HEIGHT - BOTTOM;
                    view! {
                        <svg
                            viewBox=format!("0 0 {} {HEIGHT}", frame.width)
                            width=frame.width
                            height=HEIGHT
                            role="img"
                            aria-label="Courbe des températures"
                        >
                            {degrees
                                .into_iter()
                                .map(|(y, label)| {
                                    view! {
                                        <line class="grid" x1=LEFT x2=right y1=y y2=y></line>
                                        <text class="tick" x=LEFT - 6.0 y=y + 4.0 text-anchor="end">{label}</text>
                                    }
                                })
                                .collect_view()}
                            {times
                                .into_iter()
                                .map(|(x, label)| {
                                    view! {
                                        <line class="grid" x1=x x2=x y1=TOP y2=bottom></line>
                                        <text class="tick" x=x y=HEIGHT - 6.0 text-anchor="middle">{label}</text>
                                    }
                                })
                                .collect_view()}
                            {lines
                                .into_iter()
                                .map(|(class, path, target)| {
                                    view! { <path class=format!("line {class}") class:target=target d=path></path> }
                                })
                                .collect_view()}
                        </svg>
                    }
                }}
            </div>
            <p class="legend">
                {move || {
                    drawing
                        .get()
                        .2
                        .into_iter()
                        .map(|(class, label)| view! { <span class=format!("key {class}")>{label}</span> })
                        .collect_view()
                }}
                <span class="key-note">"pointillés : consigne"</span>
            </p>
        </section>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(time: i64, nozzle: f32, bed: Option<f32>) -> Sample {
        Sample {
            time,
            nozzles: vec![Heater { current: nozzle, target: 0.0 }, Heater { current: 220.0, target: 220.0 }],
            bed: bed.map(|current| Heater { current, target: 55.0 }),
            chamber: None,
        }
    }

    #[test]
    fn the_scale_tops_the_hottest_with_room() {
        assert_eq!(scale_top(&[sample(0, 42.0, Some(55.0))]), 250.0);
        assert_eq!(scale_top(&[]), 50.0);
        assert_eq!(degree_ticks(250.0), vec![0.0, 50.0, 100.0, 150.0, 200.0, 250.0]);
        assert_eq!(degree_ticks(50.0), vec![0.0, 25.0, 50.0]);
    }

    #[test]
    fn time_ticks_fall_on_local_hours() {
        // CEST is two hours ahead of UTC: getTimezoneOffset says -120 min.
        let offset = -7200;
        let ticks = time_ticks(1_000, 4 * 3600 + 1_000, 3600, offset);
        assert_eq!(ticks, vec![3600, 7200, 10800, 14400]);
        assert_eq!(time_ticks(0, 1_800, 600, 0), vec![0, 600, 1200, 1800]);
    }

    #[test]
    fn lines_break_where_nothing_was_heard() {
        let samples = [sample(0, 40.0, Some(50.0)), sample(5, 41.0, Some(50.0)), sample(100, 42.0, Some(50.0))];
        let frame = Frame { from: 0, to: 100, top: 100.0, width: LEFT + RIGHT + 100.0 };
        let line = path(&samples, Series::Nozzle(0), false, &frame);
        assert_eq!(line.matches('M').count(), 2, "{line}");
        assert_eq!(line.matches('L').count(), 1, "{line}");
        assert_eq!(path(&samples, Series::Nozzle(0), true, &frame), "", "an off heater draws no target");
        assert_eq!(path(&samples, Series::Chamber, false, &frame), "");
    }

    #[test]
    fn points_within_a_pixel_merge() {
        let samples: Vec<Sample> = (0..100).map(|i| sample(i * 5, 40.0 + i as f32, None)).collect();
        let frame = Frame { from: 0, to: 500, top: 200.0, width: LEFT + RIGHT + 10.0 };
        let line = path(&samples, Series::Nozzle(0), false, &frame);
        assert!(line.matches('L').count() <= 10, "{line}");
        assert!(line.starts_with('M'));
    }

    #[test]
    fn series_follow_the_history() {
        assert_eq!(series_in(&[sample(0, 40.0, Some(50.0))]), vec![Series::Nozzle(0), Series::Nozzle(1), Series::Bed]);
        assert_eq!(Series::Nozzle(1).label(2), "Buse gauche");
    }
}
