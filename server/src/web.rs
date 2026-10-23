//! The web server: the page, its API, the invitations, and who may reach
//! what.
//!
//! The built page is public, as its source is; everything about the printer
//! needs the login cookie that an accepted invitation sets, or that the
//! bot's Mini App sets for the bot's owner. Requests that
//! change something, and the WebSocket, must come from the page's own
//! origin.

use std::sync::Arc;

use axum::Router;
use axum::extract::{FromRequestParts, Path, State, WebSocketUpgrade};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, COOKIE, ORIGIN, REFERRER_POLICY, SET_COOKIE};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use serde_json::json;
use tower_http::services::{ServeDir, ServeFile};
use x2d_protocol::Role;

use crate::hub::Hub;
use crate::people::Signed;
use crate::{session, webapp};

/// The cookie a login token travels in.
const COOKIE_NAME: &str = "x2d_login";

/// The routes of the server.
pub fn router(hub: Arc<Hub>) -> Router {
    let site = ServeDir::new(&hub.config.site).fallback(ServeFile::new(hub.config.site.join("index.html")));
    Router::new()
        .route("/api/me", get(me))
        .route("/api/history", get(history))
        .route("/api/thumbnail/{key}", get(thumbnail))
        .route("/ws", get(socket))
        .route("/invite/{token}", get(invitation).post(accept))
        .route("/api/telegram", post(telegram))
        .fallback_service(site)
        .with_state(hub)
}

/// A request from a signed-in person, and the token that signed them in.
pub struct Reader {
    pub signed: Signed,
    pub token: String,
}

impl FromRequestParts<Arc<Hub>> for Reader {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, hub: &Arc<Hub>) -> Result<Self, Response> {
        let refused = || (StatusCode::UNAUTHORIZED, "Réservé aux personnes invitées.").into_response();
        let token = login_token(&parts.headers).ok_or_else(refused)?;
        match hub.people.signed_in(&token) {
            Ok(Some(signed)) => Ok(Self { signed, token }),
            Ok(None) => Err(refused()),
            Err(error) => {
                tracing::error!("cannot read the people: {error:#}");
                Err(StatusCode::INTERNAL_SERVER_ERROR.into_response())
            }
        }
    }
}

fn login_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == COOKIE_NAME)
        .map(|(_, token)| token.to_owned())
}

/// Whether the request comes from the page's own origin.
fn same_origin(headers: &HeaderMap, hub: &Hub) -> bool {
    headers.get(ORIGIN).and_then(|origin| origin.to_str().ok()) == Some(hub.config.origin.as_str())
}

async fn me(reader: Reader) -> Response {
    axum::Json(json!({ "name": reader.signed.name, "role": reader.signed.role })).into_response()
}

async fn history(State(hub): State<Arc<Hub>>, _reader: Reader) -> Response {
    match hub.history_json() {
        Ok(json) => ([(CONTENT_TYPE, "application/json"), (CACHE_CONTROL, "no-store")], json).into_response(),
        Err(error) => {
            tracing::error!("cannot write the history: {error:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn thumbnail(State(hub): State<Arc<Hub>>, _reader: Reader, Path(key): Path<String>) -> Response {
    match hub.thumbnails.get(&key) {
        Some(png) => {
            ([(CONTENT_TYPE, "image/png"), (CACHE_CONTROL, "private, max-age=604800, immutable")], png).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn socket(
    State(hub): State<Arc<Hub>>,
    reader: Reader,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !same_origin(&headers, &hub) {
        return (StatusCode::FORBIDDEN, "Cette page vient d'un autre site.").into_response();
    }
    upgrade.on_upgrade(move |socket| session::run(socket, hub, reader))
}

/// The page that offers to accept an invitation. Opening the link does not
/// spend it, so that a messenger's preview leaves it whole.
async fn invitation(State(hub): State<Arc<Hub>>, Path(token): Path<String>) -> Response {
    let body = match hub.people.invited(&token) {
        Ok(Some(signed)) => format!(
            "<h1>Bonjour {}</h1><p>Cette invitation t'ouvre l'imprimante X2D en direct, {}.</p>\
             <form method=\"post\"><button>Entrer</button></form>\
             <p class=\"small\">Ce lien ne sert qu'une fois : il ouvre la page dans ce navigateur.</p>",
            escape(&signed.name),
            signed.role.as_whom()
        ),
        Ok(None) => {
            "<h1>Lien expiré</h1><p>Cette invitation a déjà servi ou a expiré. Demande un nouveau lien à Mario.</p>"
                .to_owned()
        }
        Err(error) => {
            tracing::error!("cannot read the people: {error:#}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    page(&body)
}

/// Accept an invitation: sign this browser in and open the page.
async fn accept(State(hub): State<Arc<Hub>>, headers: HeaderMap, Path(token): Path<String>) -> Response {
    if !same_origin(&headers, &hub) {
        return (StatusCode::FORBIDDEN, "Cette demande vient d'un autre site.").into_response();
    }
    match hub.people.accept(&token) {
        Ok(Some((login, signed))) => {
            tracing::info!("{} accepted an invitation", signed.name);
            login_cookie(&login).map_or_else(
                || StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                |cookie| ([(SET_COOKIE, cookie)], Redirect::to("/")).into_response(),
            )
        }
        Ok(None) => page(
            "<h1>Lien expiré</h1><p>Cette invitation a déjà servi ou a expiré. Demande un nouveau lien à Mario.</p>",
        ),
        Err(error) => {
            tracing::error!("cannot accept an invitation: {error:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Sign in the owner of the bot's chat, who opened the page as the bot's
/// Mini App: `init_data` is what Telegram signed for it.
async fn telegram(State(hub): State<Arc<Hub>>, headers: HeaderMap, init_data: String) -> Response {
    if !same_origin(&headers, &hub) {
        return (StatusCode::FORBIDDEN, "Cette demande vient d'un autre site.").into_response();
    }
    let Some(bot) = &hub.config.telegram else { return StatusCode::NOT_FOUND.into_response() };
    let Some(user) = webapp::verify(&init_data, &bot.token, crate::now()).filter(|user| user.id == bot.chat) else {
        return (StatusCode::FORBIDDEN, "Seul le propriétaire du bot entre ainsi.").into_response();
    };
    let name = if user.first_name.trim().is_empty() { "Propriétaire" } else { user.first_name.trim() };
    match hub.people.sign_in(name, Role::Operator) {
        Ok(login) => {
            tracing::info!("{name} signed in from the bot's Mini App");
            login_cookie(&login).map_or_else(
                || StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                |cookie| ([(SET_COOKIE, cookie)], StatusCode::NO_CONTENT).into_response(),
            )
        }
        Err(error) => {
            tracing::error!("cannot sign the owner in: {error:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// The header value that keeps `login` as this browser's login.
fn login_cookie(login: &str) -> Option<HeaderValue> {
    HeaderValue::from_str(&format!("{COOKIE_NAME}={login}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=31536000"))
        .ok()
}

/// A small page of the server's own, around `body`.
fn page(body: &str) -> Response {
    let html = format!(
        "<!doctype html><html lang=\"fr\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>X2D</title>\
         <style>body{{font:17px/1.5 system-ui,sans-serif;max-width:32rem;margin:15vh auto;padding:0 1.2rem;\
         color:#1d232a;background:#f6f7f9}}@media(prefers-color-scheme:dark){{body{{color:#e6e9ee;background:#14171c}}}}\
         h1{{font-size:1.6rem}}button{{font:inherit;font-weight:600;padding:.7rem 1.6rem;border:0;border-radius:.6rem;\
         background:#2f7d5b;color:#fff;cursor:pointer}}.small{{font-size:.85rem;opacity:.7}}</style></head>\
         <body>{body}</body></html>"
    );
    // `same-origin` keeps the invitation's address from other sites, while
    // the form still sends its origin, which `no-referrer` would make `null`.
    ([(CACHE_CONTROL, "no-store"), (REFERRER_POLICY, "same-origin")], Html(html)).into_response()
}

/// `text` safe inside HTML.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_login_cookie_is_found_among_others() {
        let mut headers = HeaderMap::new();
        headers.insert(COOKIE, HeaderValue::from_static("theme=dark; x2d_login=abc_-1; other=2"));
        assert_eq!(login_token(&headers).as_deref(), Some("abc_-1"));
        assert_eq!(login_token(&HeaderMap::new()), None);
    }

    #[test]
    fn names_are_escaped() {
        assert_eq!(escape("<b>\"Max\" & co</b>"), "&lt;b&gt;&quot;Max&quot; &amp; co&lt;/b&gt;");
    }
}
