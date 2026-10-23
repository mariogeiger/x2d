//! The page's link to the server: who reads, the history, and the
//! WebSocket that brings the printer and its video and takes commands.
//! Opened as the bot's Mini App, the page first signs its reader in.
//!
//! The socket reopens by itself, waiting from 1 s to 30 s between tries,
//! after checking the reader is still signed in. Each time it opens it asks
//! again for the video, if the reader wants it and the tab shows.

use std::cell::RefCell;
use std::time::Duration;

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::Closure;
use x2d_protocol::{ClientMessage, Command, Outcome, Sample, ServerMessage};

use crate::format::capitalized;
use crate::state::{Access, Connection, Notice, Page, Reader, VideoStatus};
use crate::{telegram, video};

/// How long an outcome stays on screen.
const NOTICE_TIME: Duration = Duration::from_secs(6);

struct Link {
    socket: Option<web_sys::WebSocket>,
    /// Seconds to wait before the next try.
    backoff: u32,
    /// The number of the next command.
    next_id: u32,
    /// What the server was last told about the video, since the socket
    /// opened.
    watch_sent: Option<bool>,
}

thread_local! {
    static LINK: RefCell<Link> = const { RefCell::new(Link { socket: None, backoff: 1, next_id: 1, watch_sent: None }) };
}

fn with<R>(f: impl FnOnce(&mut Link) -> R) -> R {
    LINK.with(|link| f(&mut link.borrow_mut()))
}

/// Asks the server who reads, and opens the link if they may.
pub fn start(page: Page) {
    leptos::task::spawn_local(async move {
        telegram::sign_in().await;
        let access = ask_access().await;
        let granted = matches!(access, Access::Granted(_));
        page.access.set(access);
        if granted {
            connect(page);
        }
    });
    let changed = Closure::<dyn Fn()>::new(move || sync_watch(page));
    if let Some(document) = web_sys::window().and_then(|window| window.document()) {
        _ = document.add_event_listener_with_callback("visibilitychange", changed.as_ref().unchecked_ref());
    }
    changed.forget();
}

async fn ask_access() -> Access {
    let Some(response) = get("/api/me").await else { return Access::Unreachable };
    match response.status() {
        200 => match json::<Reader>(&response).await {
            Some(reader) => Access::Granted(reader),
            None => Access::Unreachable,
        },
        401 | 403 => Access::Denied,
        _ => Access::Unreachable,
    }
}

async fn get(path: &str) -> Option<web_sys::Response> {
    let window = web_sys::window()?;
    let response = wasm_bindgen_futures::JsFuture::from(window.fetch_with_str(path)).await.ok()?;
    response.dyn_into().ok()
}

async fn json<T: serde::de::DeserializeOwned>(response: &web_sys::Response) -> Option<T> {
    let text = wasm_bindgen_futures::JsFuture::from(response.text().ok()?).await.ok()?.as_string()?;
    serde_json::from_str(&text).ok()
}

async fn load_history(page: Page) {
    let Some(response) = get("/api/history").await.filter(|response| response.status() == 200) else { return };
    if let Some(samples) = json::<Vec<Sample>>(&response).await {
        page.history.set(samples);
    }
}

fn socket_url() -> Option<String> {
    let location = web_sys::window()?.location();
    let scheme = if location.protocol().ok()? == "https:" { "wss" } else { "ws" };
    Some(format!("{scheme}://{}/ws", location.host().ok()?))
}

fn connect(page: Page) {
    let Some(socket) = socket_url().and_then(|url| web_sys::WebSocket::new(&url).ok()) else {
        retry(page);
        return;
    };
    socket.set_binary_type(web_sys::BinaryType::Arraybuffer);
    page.connection.set(Connection::Connecting);
    let opened = Closure::<dyn Fn()>::new(move || {
        page.connection.set(Connection::Open);
        with(|link| {
            link.backoff = 1;
            link.watch_sent = None;
        });
        leptos::task::spawn_local(load_history(page));
        sync_watch(page);
    });
    let received = Closure::<dyn Fn(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
        let data = event.data();
        if let Some(text) = data.as_string() {
            if let Ok(message) = serde_json::from_str::<ServerMessage>(&text) {
                receive(page, message);
            }
        } else if let Ok(buffer) = data.dyn_into::<js_sys::ArrayBuffer>() {
            video::chunk(page, &js_sys::Uint8Array::new(&buffer).to_vec());
        }
    });
    let closed = Closure::<dyn Fn()>::new(move || closed(page));
    socket.set_onopen(Some(opened.as_ref().unchecked_ref()));
    socket.set_onmessage(Some(received.as_ref().unchecked_ref()));
    socket.set_onclose(Some(closed.as_ref().unchecked_ref()));
    opened.forget();
    received.forget();
    closed.forget();
    with(|link| link.socket = Some(socket));
}

fn closed(page: Page) {
    with(|link| {
        link.socket = None;
        link.watch_sent = None;
    });
    video::stop();
    if page.video.get_untracked() == VideoStatus::Playing {
        page.video.set(VideoStatus::Starting);
    }
    for (id, command) in page.pending.get_untracked() {
        notify(page, id, command, &Outcome::Failed("connexion perdue".into()));
    }
    page.pending.set(Vec::new());
    retry(page);
}

fn retry(page: Page) {
    let wait = with(|link| {
        let wait = link.backoff;
        link.backoff = (link.backoff * 2).min(30);
        wait
    });
    page.connection.set(Connection::Retrying { seconds: wait });
    set_timeout(
        move || {
            leptos::task::spawn_local(async move {
                match ask_access().await {
                    Access::Denied => page.access.set(Access::Denied),
                    Access::Granted(reader) => {
                        page.access.set(Access::Granted(reader));
                        connect(page);
                    }
                    _ => retry(page),
                }
            });
        },
        Duration::from_secs(u64::from(wait)),
    );
}

fn send(message: &ClientMessage) -> bool {
    let Ok(text) = serde_json::to_string(message) else { return false };
    with(|link| {
        link.socket
            .as_ref()
            .filter(|socket| socket.ready_state() == web_sys::WebSocket::OPEN)
            .is_some_and(|socket| socket.send_with_str(&text).is_ok())
    })
}

fn receive(page: Page, message: ServerMessage) {
    match message {
        ServerMessage::Welcome { name, role } => page.access.set(Access::Granted(Reader { name, role })),
        ServerMessage::Printer(printer) => page.printer.set(Some(printer)),
        ServerMessage::Sample(sample) => page.record(sample),
        ServerMessage::Video(config) => video::configure(page, config),
        ServerMessage::VideoLost { reason } => video::lost(page, reason),
        ServerMessage::Done { id, outcome } => {
            let command =
                page.pending.with_untracked(|pending| pending.iter().find(|(sent, _)| *sent == id).map(|p| p.1));
            page.pending.update(|pending| pending.retain(|(sent, _)| *sent != id));
            if let Some(command) = command {
                notify(page, id, command, &outcome);
            }
        }
    }
}

/// Tells the server whether to send the video: when the reader wants it,
/// the tab shows and the browser can decode it, once the socket is open.
pub fn sync_watch(page: Page) {
    let visible = web_sys::window().and_then(|window| window.document()).is_none_or(|document| !document.hidden());
    let wanted = page.watching.get_untracked();
    let on = wanted && visible && video::supported();
    let status = page.video.get_untracked();
    match (on, wanted) {
        _ if status == VideoStatus::Unsupported => {}
        (false, false) => page.video.set(VideoStatus::Off),
        (false, true) => page.video.set(VideoStatus::Hidden),
        (true, _) if matches!(status, VideoStatus::Off | VideoStatus::Hidden) => page.video.set(VideoStatus::Starting),
        _ => {}
    }
    if with(|link| link.watch_sent) == Some(on) {
        return;
    }
    if !on {
        video::stop();
    }
    if send(&ClientMessage::Watch { on }) {
        with(|link| link.watch_sent = Some(on));
    }
}

/// Sends `command` to the printer, and tells the reader what became of it.
pub fn command(page: Page, command: Command) {
    let id = with(|link| {
        let id = link.next_id;
        link.next_id += 1;
        id
    });
    if send(&ClientMessage::Command { id, command }) {
        page.pending.update(|pending| pending.push((id, command)));
    } else {
        notify(page, id, command, &Outcome::Failed("pas de connexion au serveur".into()));
    }
}

fn notify(page: Page, id: u32, command: Command, outcome: &Outcome) {
    let label = capitalized(command.label());
    let (text, ok) = match outcome {
        Outcome::Done => (format!("{label} : fait."), true),
        Outcome::Failed(reason) => (format!("{label} : échec, {reason}."), false),
    };
    page.notices.update(|notices| notices.push(Notice { id, text, ok }));
    set_timeout(move || page.notices.update(|notices| notices.retain(|notice| notice.id != id)), NOTICE_TIME);
}
