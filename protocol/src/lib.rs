//! What the x2d server and its page say to each other.
//!
//! The server watches one Bambu Lab printer and tells each page what it sees:
//! the printer as a [`Printer`], its temperatures as [`Sample`]s, and its
//! camera as H.264 [`video`] chunks. A page asks for the video and, when its
//! reader operates the printer, sends a [`Command`].

mod messages;
mod printer;
pub mod video;

pub use messages::{ClientMessage, Command, Outcome, Role, ServerMessage};
pub use printer::{
    Alert, AmsUnit, Fans, Heater, Job, JobState, Nozzle, Printer, Sample, Slot, Tray, speed_label, stage_label,
};
