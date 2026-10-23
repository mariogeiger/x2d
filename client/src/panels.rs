//! The cards that tell the printer's state: its print, its heaters, its AMS,
//! its alerts, and its fans and network.

use leptos::prelude::*;
use x2d_protocol::{Alert, AmsUnit, Heater, JobState, Printer, Tray, speed_label};

use crate::format::{capitalized, decimal, degrees, duration, moment, swatch, wifi_quality};
use crate::state::Page;

/// Reads the printer, or a default one before the first word of the server.
fn printer(page: Page) -> impl Fn() -> Printer + Copy {
    move || page.printer.get().unwrap_or_default()
}

/// The print: its name and picture, how far it is, and when it ends.
#[component]
pub fn JobCard() -> impl IntoView {
    let page = expect_context::<Page>();
    let printer = printer(page);
    view! {
        <section class="card job">
            {move || {
                let printer = printer();
                let Some(job) = printer.job.clone() else {
                    return view! {
                        <h2>"Impression"</h2>
                        <p class="muted">{format!("Aucune impression ({}).", printer.state.label())}</p>
                    }
                        .into_any();
                };
                let underway = printer.state.underway();
                let reported = if printer.reported_at > 0 {
                    printer.reported_at as f64 * 1000.0
                } else {
                    page.clock.get()
                };
                let end = reported + f64::from(job.remaining_minutes) * 60_000.0;
                let state = printer.state.clone();
                view! {
                    <div class="job-head">
                        {job
                            .thumbnail
                            .clone()
                            .map(|src| view! { <img class="thumbnail" src=src alt="Aperçu du plateau" /> })}
                        <div class="job-title">
                            <h2>{job.name.clone()}</h2>
                            <p class="muted">
                                {capitalized(&state.label())}
                                {printer.speed.and_then(speed_label).map(|speed| format!(" · vitesse {speed}"))}
                            </p>
                        </div>
                        <p class="percent">{format!("{} %", job.percent)}</p>
                    </div>
                    <div class="progress" class:paused=state == JobState::Paused class:failed=state == JobState::Failed>
                        <div class="bar" style:width=format!("{}%", job.percent.min(100))></div>
                    </div>
                    <dl class="facts">
                        <div>
                            <dt>"Reste"</dt>
                            <dd>{if underway { duration(job.remaining_minutes) } else { "—".into() }}</dd>
                        </div>
                        <div>
                            <dt>"Fin"</dt>
                            <dd>{if underway { format!("vers {}", moment(end, page.clock.get())) } else { "—".into() }}</dd>
                        </div>
                        <div>
                            <dt>"Couche"</dt>
                            <dd>{format!("{} / {}", job.layer, job.total_layers)}</dd>
                        </div>
                    </dl>
                }
                    .into_any()
            }}
        </section>
    }
}

/// A heater's temperatures, or its temperature and `idle`, the word for it
/// without a target.
fn heater_text(heater: Heater, idle: &str) -> String {
    if heater.target > 0.0 {
        format!("{} / {}", degrees(heater.current), degrees(heater.target))
    } else {
        format!("{} · {idle}", degrees(heater.current))
    }
}

/// The heaters: each nozzle, the bed and the chamber.
#[component]
pub fn HeatersCard() -> impl IntoView {
    let page = expect_context::<Page>();
    let printer = printer(page);
    view! {
        <section class="card heaters">
            <h2>"Chauffe"</h2>
            <ul class="rows">
                {move || {
                    let printer = printer();
                    let count = printer.nozzles.len();
                    let nozzles = printer
                        .nozzles
                        .iter()
                        .map(|nozzle| {
                            let details = [
                                nozzle.diameter.map(|diameter| format!("{} mm", decimal(diameter, 1))),
                                nozzle.kind.clone(),
                                nozzle.loaded.map(|slot| format!("filament {}", slot.label())),
                            ]
                            .into_iter()
                            .flatten()
                            .collect::<Vec<_>>()
                            .join(" · ");
                            view! {
                                <li class:active=nozzle.active>
                                    <span class=format!("key nozzle{}", nozzle.id)>{nozzle.label(count)}</span>
                                    {nozzle.active.then(|| view! { <span class="badge">"active"</span> })}
                                    <span class="detail">{details}</span>
                                    <span class="value">{heater_text(nozzle.heater, "éteinte")}</span>
                                </li>
                            }
                        })
                        .collect_view();
                    let others = [("Plateau", "bed", "éteint", printer.bed), ("Chambre", "chamber", "non chauffée", printer.chamber)]
                        .into_iter()
                        .filter_map(|(name, class, idle, heater)| heater.map(|heater| (name, class, idle, heater)))
                        .map(|(name, class, idle, heater)| {
                            view! {
                                <li>
                                    <span class=format!("key {class}")>{name}</span>
                                    <span class="detail"></span>
                                    <span class="value">{heater_text(heater, idle)}</span>
                                </li>
                            }
                        })
                        .collect_view();
                    (nozzles, others)
                }}
            </ul>
        </section>
    }
}

fn tray_view(tray: &Tray, printer: &Printer) -> impl IntoView + use<> {
    let count = printer.nozzles.len();
    let loaded_in =
        printer.nozzles.iter().find(|nozzle| nozzle.loaded == Some(tray.slot)).map(|nozzle| nozzle.label(count));
    let background = swatch(&tray.colors);
    let empty = tray.material.is_none();
    let material = tray.material.clone().unwrap_or_else(|| "vide".into());
    let name = tray.name.clone().filter(|name| Some(name) != tray.material.as_ref());
    let remain = tray.remain.filter(|remain| *remain <= 100).map(|remain| format!("{remain} %"));
    let loaded = loaded_in.is_some();
    view! {
        <li class="tray" class:loaded=loaded class:empty=empty>
            <span class="swatch" style:background=background.unwrap_or_default()></span>
            <span class="slot">{tray.slot.label()}</span>
            <span class="material">{material}</span>
            {name.map(|name| view! { <span class="name">{name}</span> })}
            {remain.map(|remain| view! { <span class="remain">{remain}</span> })}
            {loaded_in.map(|nozzle| view! { <span class="badge">{nozzle.to_lowercase()}</span> })}
        </li>
    }
}

fn unit_view(unit: &AmsUnit, printer: &Printer) -> impl IntoView + use<> {
    let facts = [
        unit.humidity.map(|humidity| format!("{humidity} % d'humidité")),
        unit.temperature.map(|temperature| format!("{} °C", decimal(temperature, 1))),
        unit.drying_minutes
            .filter(|minutes| *minutes > 0)
            .map(|minutes| format!("séchage, encore {}", duration(minutes))),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ");
    view! {
        <div class="unit">
            <h3>{unit.label()} <span class="muted">{facts}</span></h3>
            <ul class="trays">{unit.trays.iter().map(|tray| tray_view(tray, printer)).collect_view()}</ul>
        </div>
    }
}

/// The AMS units and their trays, and the spools hung outside.
#[component]
pub fn AmsCard() -> impl IntoView {
    let page = expect_context::<Page>();
    let printer = printer(page);
    view! {
        <section class="card ams">
            <h2>"Filaments"</h2>
            {move || {
                let printer = printer();
                if printer.ams.is_empty() && printer.external.is_empty() {
                    return view! { <p class="muted">"Aucun AMS ni bobine externe signalés."</p> }.into_any();
                }
                let units = printer.ams.iter().map(|unit| unit_view(unit, &printer)).collect_view();
                let external = (!printer.external.is_empty()).then(|| {
                    view! {
                        <div class="unit">
                            <h3>"Bobines externes"</h3>
                            <ul class="trays">
                                {printer.external.iter().map(|tray| tray_view(tray, &printer)).collect_view()}
                            </ul>
                        </div>
                    }
                });
                view! { {units} {external} }.into_any()
            }}
        </section>
    }
}

fn alert_view(alert: &Alert, kind: &'static str) -> impl IntoView + use<> {
    let text = alert.text.clone().unwrap_or_else(|| "texte inconnu".into());
    view! {
        <li class=kind>
            <span class="code">{alert.code.clone()}</span>
            <span>{text}</span>
        </li>
    }
}

/// The printer's error and its health alerts.
#[component]
pub fn AlertsCard() -> impl IntoView {
    let page = expect_context::<Page>();
    let printer = printer(page);
    view! {
        <section class="card alerts">
            <h2>"Alertes"</h2>
            {move || {
                let printer = printer();
                if printer.alerts.is_empty() && printer.error.is_none() {
                    return view! { <p class="muted">"Aucune alerte."</p> }.into_any();
                }
                view! {
                    <ul class="alert-list">
                        {printer.error.as_ref().map(|error| alert_view(error, "error"))}
                        {printer.alerts.iter().map(|alert| alert_view(alert, "warning")).collect_view()}
                    </ul>
                }
                    .into_any()
            }}
        </section>
    }
}

/// The fans, the speed level and the Wi-Fi signal.
#[component]
pub fn MachineCard() -> impl IntoView {
    let page = expect_context::<Page>();
    let printer = printer(page);
    let percent = |value: Option<u8>| value.map_or_else(|| "—".to_owned(), |value| format!("{value} %"));
    view! {
        <section class="card machine">
            <h2>"Machine"</h2>
            {move || {
                let printer = printer();
                let fans = printer.fans;
                let wifi = printer
                    .wifi_dbm
                    .map_or_else(|| "—".to_owned(), |dbm| format!("{dbm} dBm, {}", wifi_quality(dbm)));
                let speed = printer.speed.and_then(speed_label).map_or_else(|| "—".to_owned(), capitalized);
                view! {
                    <dl class="facts grid">
                        <div><dt>"Ventilateur pièce"</dt><dd>{percent(fans.part)}</dd></div>
                        <div><dt>"Auxiliaire"</dt><dd>{percent(fans.aux)}</dd></div>
                        <div><dt>"Chambre"</dt><dd>{percent(fans.chamber)}</dd></div>
                        <div><dt>"Heatbreak"</dt><dd>{percent(fans.heatbreak)}</dd></div>
                        <div><dt>"Vitesse"</dt><dd>{speed}</dd></div>
                        <div><dt>"Wi-Fi"</dt><dd>{wifi}</dd></div>
                    </dl>
                }
            }}
        </section>
    }
}
