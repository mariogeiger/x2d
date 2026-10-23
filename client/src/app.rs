//! The page as a whole: who may read it, its header, and the board of
//! cards around the camera.

use std::time::Duration;

use leptos::prelude::*;
use x2d_protocol::stage_label;

use crate::chart::TemperatureChart;
use crate::controls::{ConfirmDialog, Controls, Notices};
use crate::format::{capitalized, moment};
use crate::link;
use crate::panels::{AlertsCard, AmsCard, HeatersCard, JobCard, MachineCard};
use crate::state::{Access, Connection, Page};
use crate::video::Camera;

/// How often the page's clock ticks.
const TICK: Duration = Duration::from_secs(10);

/// The page.
#[component]
pub fn App() -> impl IntoView {
    let page = Page::new();
    provide_context(page);
    link::start(page);
    if let Ok(ticking) = set_interval_with_handle(move || page.clock.set(js_sys::Date::now()), TICK) {
        on_cleanup(move || ticking.clear());
    }
    move || match page.access.get() {
        Access::Checking => view! { <main class="gate"><p class="muted">"Chargement…"</p></main> }.into_any(),
        Access::Denied => view! {
            <main class="gate">
                <h1>"X2D"</h1>
                <p>"Cette page est réservée aux personnes invitées."</p>
                <p class="muted">"Demande un lien d'invitation à Mario ; il t'ouvrira la page sur ce navigateur."</p>
            </main>
        }
        .into_any(),
        Access::Unreachable => view! {
            <main class="gate">
                <h1>"X2D"</h1>
                <p>"Le serveur ne répond pas."</p>
                <button on:click=|_| reload()>"Réessayer"</button>
            </main>
        }
        .into_any(),
        Access::Granted(_) => view! { <Board /> }.into_any(),
    }
}

fn reload() {
    if let Some(window) = web_sys::window() {
        _ = window.location().reload();
    }
}

/// The header: the printer's state, the link's, and who reads.
#[component]
fn Header() -> impl IntoView {
    let page = expect_context::<Page>();
    let state = move || {
        page.printer.with(|printer| {
            let Some(printer) = printer else { return "en attente de l'imprimante".to_owned() };
            let state = printer.state.label();
            match printer.stage.and_then(stage_label).filter(|stage| *stage != state) {
                Some(stage) if printer.state.underway() => format!("{} · {stage}", capitalized(&state)),
                _ => capitalized(&state),
            }
        })
    };
    let online = move || page.printer.with(|printer| printer.as_ref().is_some_and(|p| p.online));
    let connection = move || match page.connection.get() {
        Connection::Connecting => "connexion…".to_owned(),
        Connection::Open => "connecté".to_owned(),
        Connection::Retrying { seconds } => format!("hors ligne, nouvel essai dans {seconds} s"),
    };
    let reader = move || page.reader().map(|reader| format!("{} · {}", reader.name, reader.role.label()));
    view! {
        <header class="top">
            <h1>"X2D"</h1>
            <p class="state">
                <span class="dot" class:online=online title=move || if online() { "imprimante en ligne" } else { "imprimante hors ligne" }></span>
                {state}
            </p>
            <p class="who muted">
                <span class:offline=move || page.connection.get() != Connection::Open>{connection}</span>
                {move || reader().map(|reader| view! { <span>{reader}</span> })}
            </p>
        </header>
    }
}

/// The banner shown while the printer does not answer.
#[component]
fn OfflineBanner() -> impl IntoView {
    let page = expect_context::<Page>();
    move || {
        page.printer.with(|printer| {
            let printer = printer.as_ref().filter(|printer| !printer.online)?;
            let text = if printer.reported_at > 0 {
                format!(
                    "L'imprimante ne répond pas ; dernières nouvelles à {}.",
                    moment(printer.reported_at as f64 * 1000.0, page.clock.get())
                )
            } else {
                "L'imprimante n'a encore rien envoyé au serveur.".to_owned()
            };
            Some(view! { <p class="banner">{text}</p> })
        })
    }
}

/// The board: the camera and the chart beside the cards.
#[component]
fn Board() -> impl IntoView {
    let page = expect_context::<Page>();
    let operator = move || page.reader().is_some_and(|reader| reader.role.commands());
    view! {
        <Header />
        <OfflineBanner />
        <main class="board">
            <div class="main-column">
                <Camera />
                <Show when=operator>
                    <Controls />
                </Show>
                <TemperatureChart />
            </div>
            <div class="side-column">
                <JobCard />
                <HeatersCard />
                <AmsCard />
                <AlertsCard />
                <MachineCard />
            </div>
        </main>
        <ConfirmDialog />
        <Notices />
    }
}
