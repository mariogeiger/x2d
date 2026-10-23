//! The x2d page: Mario's Bambu Lab X2D, live, as its server tells it.

mod app;
mod chart;
mod controls;
mod format;
mod link;
mod panels;
mod state;
mod video;

fn main() {
    std::panic::set_hook(Box::new(|info| web_sys::console::error_1(&info.to_string().into())));
    leptos::mount::mount_to_body(app::App);
}
