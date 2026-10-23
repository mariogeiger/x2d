//! One page's WebSocket: the printer as it changes, its history as it
//! grows, the video while the page watches, and the commands of operators.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, Utf8Bytes, WebSocket};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use x2d_protocol::{ClientMessage, Command, Outcome, ServerMessage};

use crate::camera::{Event, Viewer};
use crate::commands;
use crate::hub::Hub;
use crate::web::Reader;

/// How often the session checks that its reader is still invited, and pings.
const CHECK_EVERY: Duration = Duration::from_secs(30);

/// Serve one page until it leaves or its reader is revoked.
pub async fn run(socket: WebSocket, hub: Arc<Hub>, reader: Reader) {
    if let Err(error) = serve(socket, &hub, &reader).await {
        tracing::debug!("a page of {} left: {error:#}", reader.signed.name);
    }
}

async fn serve(socket: WebSocket, hub: &Arc<Hub>, reader: &Reader) -> anyhow::Result<()> {
    let (mut sink, mut stream) = socket.split();
    let mut printer = hub.printer.clone();
    printer.mark_changed();
    let mut samples = hub.samples.subscribe();
    let mut viewer: Option<Viewer> = None;
    let (done, mut finished) = mpsc::channel::<(u32, Outcome)>(8);
    let mut check = tokio::time::interval_at(tokio::time::Instant::now() + CHECK_EVERY, CHECK_EVERY);
    let welcome = ServerMessage::Welcome { name: reader.signed.name.clone(), role: reader.signed.role };
    sink.send(text(&welcome)?).await?;
    loop {
        tokio::select! {
            changed = printer.changed() => {
                changed?;
                let seen = ServerMessage::Printer((**printer.borrow_and_update()).clone());
                sink.send(text(&seen)?).await?;
            }
            sample = samples.recv() => match sample {
                Ok(sample) => sink.send(text(&ServerMessage::Sample(sample))?).await?,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(closed) => return Err(closed.into()),
            },
            event = next_event(&mut viewer) => match event {
                Some(Event::Stream(stream)) => sink.send(text(&ServerMessage::Video(stream.config.clone()))?).await?,
                Some(Event::Frame(frame)) => sink.send(Message::Binary(frame.message)).await?,
                Some(Event::Lost(reason)) => sink.send(text(&ServerMessage::VideoLost { reason })?).await?,
                None => viewer = None,
            },
            outcome = finished.recv() => {
                if let Some((id, outcome)) = outcome {
                    sink.send(text(&ServerMessage::Done { id, outcome })?).await?;
                }
            }
            _ = check.tick() => {
                if hub.people.signed_in(&reader.token)?.as_ref() != Some(&reader.signed) {
                    tracing::info!("{} was revoked: their page closes", reader.signed.name);
                    sink.send(Message::Close(None)).await?;
                    return Ok(());
                }
                sink.send(Message::Ping(Default::default())).await?;
            }
            message = stream.next() => {
                let Some(message) = message else { return Ok(()) };
                match message? {
                    Message::Text(message) => match serde_json::from_str::<ClientMessage>(&message) {
                        Ok(ClientMessage::Watch { on }) => {
                            if !on {
                                viewer = None;
                            } else if viewer.is_none() {
                                viewer = Some(hub.camera.watch());
                            }
                        }
                        Ok(ClientMessage::Command { id, command }) => {
                            tokio::spawn(command_task(Arc::clone(hub), reader.token.clone(), id, command, done.clone()));
                        }
                        Err(error) => tracing::debug!("a page sent what is not understood: {error}"),
                    },
                    Message::Close(_) => return Ok(()),
                    _ => {}
                }
            }
        }
    }
}

/// The next video event, if the page watches.
async fn next_event(viewer: &mut Option<Viewer>) -> Option<Event> {
    match viewer {
        Some(viewer) => viewer.next().await,
        None => std::future::pending().await,
    }
}

/// Do `command` for the reader of `token`, if they may and the printer can,
/// and report its outcome under `id`.
async fn command_task(hub: Arc<Hub>, token: String, id: u32, command: Command, done: mpsc::Sender<(u32, Outcome)>) {
    // Who sends a command is read again: a revoked or demoted operator may
    // still have their page open.
    let signed = match hub.people.signed_in(&token) {
        Ok(Some(signed)) if signed.role.commands() => signed,
        _ => {
            let _ = done.send((id, Outcome::Failed("réservé aux opérateurs".into()))).await;
            return;
        }
    };
    let printer = Arc::clone(&hub.printer.borrow());
    let outcome = match commands::admissible(command, &printer) {
        Ok(()) => commands::send(&hub.link, command).await,
        Err(why) => Outcome::Failed(why),
    };
    tracing::info!("{} asked to {}: {outcome:?}", signed.name, command.label());
    hub.log.note(&signed.name, command, &outcome);
    let _ = done.send((id, outcome)).await;
}

fn text(message: &ServerMessage) -> anyhow::Result<Message> {
    Ok(Message::Text(Utf8Bytes::from(serde_json::to_string(message)?)))
}
