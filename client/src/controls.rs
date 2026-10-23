//! What an operator does to the printer: the light in one click; pause,
//! resume and stop once confirmed. The server checks the role again, so
//! these only spare a spectator buttons they could not use.

use leptos::prelude::*;
use x2d_protocol::{Command, JobState};

use crate::format::capitalized;
use crate::link;
use crate::state::{Connection, Page};

/// Whether `command` waits for its answer, or one of its kind does: the
/// light, or the commands of the print.
fn waiting(page: Page, command: Command) -> bool {
    let light = matches!(command, Command::Light { .. });
    page.pending.with(|pending| pending.iter().any(|(_, sent)| matches!(sent, Command::Light { .. }) == light))
}

/// The operator's buttons.
#[component]
pub fn Controls() -> impl IntoView {
    let page = expect_context::<Page>();
    let reachable = move || {
        page.connection.get() == Connection::Open
            && page.printer.with(|printer| printer.as_ref().is_some_and(|p| p.online))
    };
    let state = move || page.printer.with(|printer| printer.as_ref().map(|p| p.state.clone()).unwrap_or_default());
    let light = move || page.printer.with(|printer| printer.as_ref().and_then(|p| p.light));
    let toggle_light = move |_| link::command(page, Command::Light { on: light() != Some(true) });
    let ask = move |command: Command| move |_| page.confirming.set(Some(command));
    view! {
        <section class="card controls">
            <h2>"Commandes"</h2>
            <div class="buttons">
                <button
                    class="light"
                    class:on=move || light() == Some(true)
                    disabled=move || !reachable() || waiting(page, Command::Light { on: true })
                    on:click=toggle_light
                >
                    {move || match light() {
                        Some(true) => "Lumière allumée",
                        Some(false) => "Lumière éteinte",
                        None => "Lumière",
                    }}
                </button>
                <button
                    disabled=move || !reachable() || state() != JobState::Printing || waiting(page, Command::Pause)
                    on:click=ask(Command::Pause)
                >
                    "Pause"
                </button>
                <button
                    disabled=move || !reachable() || state() != JobState::Paused || waiting(page, Command::Resume)
                    on:click=ask(Command::Resume)
                >
                    "Reprendre"
                </button>
                <button
                    class="danger"
                    disabled=move || !reachable() || !state().underway() || waiting(page, Command::Stop)
                    on:click=ask(Command::Stop)
                >
                    "Arrêter"
                </button>
            </div>
        </section>
    }
}

/// The question an operator answers before a command of the print goes.
#[component]
pub fn ConfirmDialog() -> impl IntoView {
    let page = expect_context::<Page>();
    let cancel = move |_| page.confirming.set(None);
    let confirm = move |_| {
        if let Some(command) = page.confirming.get_untracked() {
            page.confirming.set(None);
            link::command(page, command);
        }
    };
    move || {
        page.confirming.get().map(|command| {
            let warning = (command == Command::Stop).then(|| {
                view! { <p class="warning">"L'impression sera perdue : elle ne pourra pas reprendre."</p> }
            });
            view! {
                <div class="overlay" on:click=cancel>
                    <div
                        class="dialog"
                        role="alertdialog"
                        aria-modal="true"
                        on:click=|event| event.stop_propagation()
                    >
                        <h2>"Confirmer"</h2>
                        <p>{format!("{} ?", capitalized(command.label()))}</p>
                        {warning}
                        <div class="buttons">
                            <button on:click=cancel>"Annuler"</button>
                            <button class="primary" class:danger=command == Command::Stop on:click=confirm>
                                {capitalized(command.label())}
                            </button>
                        </div>
                    </div>
                </div>
            }
        })
    }
}

/// What became of the commands lately sent.
#[component]
pub fn Notices() -> impl IntoView {
    let page = expect_context::<Page>();
    view! {
        <div class="notices" aria-live="polite">
            {move || {
                page.notices
                    .get()
                    .into_iter()
                    .map(|notice| view! { <p class="notice" class:failed=!notice.ok>{notice.text}</p> })
                    .collect_view()
            }}
        </div>
    }
}
