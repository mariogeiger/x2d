//! The camera on the page: H.264 frames decoded by the browser's WebCodecs
//! and drawn on a canvas, live.
//!
//! A [`VideoConfig`] starts the video over: the decoder is made anew from
//! it, then fed from the next key frame. A decoder that falls behind, by
//! more than [`MAX_QUEUE`] frames, is dropped for a fresh one at the next
//! key frame, so that the picture stays live rather than complete; one that
//! fails is made anew the same way, up to [`MAX_FAILURES`] times in a row.

use std::cell::{Cell, RefCell};

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::{Closure, JsValue};
use x2d_protocol::video::{Chunk, VideoConfig};

use crate::link;
use crate::state::{Page, VideoStatus};

/// How many frames may wait in the decoder before it is dropped.
const MAX_QUEUE: u32 = 30;
/// How many decoders in a row may fail before the page gives up on a
/// configuration.
const MAX_FAILURES: u32 = 3;

struct Decoder {
    decoder: web_sys::VideoDecoder,
    _output: Closure<dyn FnMut(web_sys::VideoFrame)>,
    _error: Closure<dyn FnMut(JsValue)>,
}

impl Drop for Decoder {
    fn drop(&mut self) {
        if self.decoder.state() != web_sys::CodecState::Closed {
            _ = self.decoder.close();
        }
    }
}

#[derive(Default)]
struct Player {
    canvas: Option<web_sys::HtmlCanvasElement>,
    config: Option<VideoConfig>,
    decoder: Option<Decoder>,
    /// Whether frames are skipped until the next key frame.
    waiting_for_key: bool,
    /// Decoders failed in a row under this configuration.
    failures: u32,
}

thread_local! {
    static PLAYER: RefCell<Player> = RefCell::new(Player::default());
    /// Set by a decoder's error callback, which runs on its own.
    static BROKEN: Cell<bool> = const { Cell::new(false) };
    /// Set once a decoder outputs a frame.
    static DECODED: Cell<bool> = const { Cell::new(false) };
}

/// Whether this browser has WebCodecs' video decoder.
pub fn supported() -> bool {
    web_sys::window().is_some_and(|window| js_sys::Reflect::has(&window, &"VideoDecoder".into()).unwrap_or(false))
}

/// Starts the video over with `config`.
pub fn configure(page: Page, config: VideoConfig) {
    if !supported() {
        page.video.set(VideoStatus::Unsupported);
        return;
    }
    PLAYER.with(|player| {
        let mut player = player.borrow_mut();
        player.decoder = None;
        player.config = Some(config);
        player.waiting_for_key = true;
        player.failures = 0;
    });
    if page.video.get_untracked() != VideoStatus::Playing {
        page.video.set(VideoStatus::Starting);
    }
}

/// Forgets the video until the next configuration.
pub fn stop() {
    PLAYER.with(|player| {
        let mut player = player.borrow_mut();
        player.decoder = None;
        player.config = None;
    });
}

/// The camera stopped, for `reason`.
pub fn lost(page: Page, reason: String) {
    stop();
    page.video.set(VideoStatus::Lost(reason));
}

/// Decodes the frame a binary message carries.
pub fn chunk(page: Page, message: &[u8]) {
    let Some(chunk) = Chunk::decode(message) else { return };
    PLAYER.with(|player| decode(page, &mut player.borrow_mut(), chunk));
}

fn decode(page: Page, player: &mut Player, chunk: Chunk<'_>) {
    if DECODED.replace(false) {
        player.failures = 0;
    }
    if BROKEN.replace(false) {
        player.decoder = None;
        player.waiting_for_key = true;
        player.failures += 1;
        if player.failures >= MAX_FAILURES {
            player.config = None;
            page.video.set(VideoStatus::Lost("ce navigateur ne décode pas ce flux".into()));
        }
    }
    if player.config.is_none() {
        return;
    }
    if let Some(decoder) = &player.decoder
        && decoder.decoder.decode_queue_size() > MAX_QUEUE
    {
        player.decoder = None;
        player.waiting_for_key = true;
    }
    if player.waiting_for_key && !chunk.key {
        return;
    }
    if player.decoder.is_none() {
        player.decoder = new_decoder(page, player);
    }
    let Some(decoder) = &player.decoder else { return };
    let kind = if chunk.key { web_sys::EncodedVideoChunkType::Key } else { web_sys::EncodedVideoChunkType::Delta };
    let init = web_sys::EncodedVideoChunkInit::new_with_u8_array(&js_sys::Uint8Array::from(chunk.data), 0, kind);
    init.set_timestamp_f64(chunk.timestamp_us as f64);
    match web_sys::EncodedVideoChunk::new(&init).and_then(|encoded| decoder.decoder.decode(&encoded)) {
        Ok(()) => player.waiting_for_key = false,
        Err(_) => BROKEN.set(true),
    }
}

fn new_decoder(page: Page, player: &Player) -> Option<Decoder> {
    let config = player.config.as_ref()?;
    let canvas = player.canvas.clone()?;
    let context: web_sys::CanvasRenderingContext2d = canvas.get_context("2d").ok()??.dyn_into().ok()?;
    let status = page.video;
    let output = Closure::<dyn FnMut(web_sys::VideoFrame)>::new(move |frame: web_sys::VideoFrame| {
        let (width, height) = (frame.display_width(), frame.display_height());
        if canvas.width() != width || canvas.height() != height {
            canvas.set_width(width);
            canvas.set_height(height);
        }
        _ = context.draw_image_with_video_frame(&frame, 0.0, 0.0);
        frame.close();
        DECODED.set(true);
        if status.get_untracked() != VideoStatus::Playing {
            status.set(VideoStatus::Playing);
        }
    });
    let error = Closure::<dyn FnMut(JsValue)>::new(move |error: JsValue| {
        web_sys::console::warn_2(&"le décodeur vidéo a échoué :".into(), &error);
        BROKEN.set(true);
    });
    let init = web_sys::VideoDecoderInit::new(error.as_ref().unchecked_ref(), output.as_ref().unchecked_ref());
    let decoder = web_sys::VideoDecoder::new(&init).ok()?;
    let settings = web_sys::VideoDecoderConfig::new(&config.codec);
    settings.set_description_u8_array(&js_sys::Uint8Array::from(config.description.as_slice()));
    settings.set_coded_width(config.width);
    settings.set_coded_height(config.height);
    settings.set_optimize_for_latency(true);
    if decoder.configure(&settings).is_err() {
        _ = decoder.close();
        BROKEN.set(true);
        return None;
    }
    Some(Decoder { decoder, _output: output, _error: error })
}

/// The camera's picture, with what it is waiting for and the switch that
/// turns it off.
#[component]
pub fn Camera() -> impl IntoView {
    let page = expect_context::<Page>();
    let canvas = NodeRef::<leptos::html::Canvas>::new();
    Effect::new(move |_| {
        if let Some(element) = canvas.get() {
            PLAYER.with(|player| player.borrow_mut().canvas = Some(element));
        }
    });
    if !supported() {
        page.video.set(VideoStatus::Unsupported);
    }
    let caption = move || {
        match page.video.get() {
        VideoStatus::Playing => None,
        VideoStatus::Off => Some("Vidéo coupée.".to_owned()),
        VideoStatus::Hidden => Some("Vidéo en pause tant que l'onglet est caché.".to_owned()),
        VideoStatus::Starting => Some("Connexion à la caméra…".to_owned()),
        VideoStatus::Lost(reason) => Some(format!("Caméra indisponible : {reason}.")),
        VideoStatus::Unsupported => Some(
            "Ce navigateur ne sait pas décoder la vidéo (WebCodecs manque). Essaie un Chrome, un Edge ou un Safari récent."
                .to_owned(),
        ),
    }
    };
    let toggle = move |_| {
        page.watching.update(|on| *on = !*on);
        link::sync_watch(page);
    };
    view! {
        <section class="camera">
            <div class="screen" class:live=move || page.video.get() == VideoStatus::Playing>
                <canvas node_ref=canvas width="1920" height="1080"></canvas>
                {move || caption().map(|text| view! { <p class="caption">{text}</p> })}
            </div>
            <div class="camera-bar">
                <span class="live-dot" class:on=move || page.video.get() == VideoStatus::Playing></span>
                <span>"Caméra"</span>
                <button
                    class="small"
                    on:click=toggle
                    disabled=move || page.video.get() == VideoStatus::Unsupported
                >
                    {move || if page.watching.get() { "Couper la vidéo" } else { "Lancer la vidéo" }}
                </button>
            </div>
        </section>
    }
}
