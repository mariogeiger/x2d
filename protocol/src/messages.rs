//! The messages of the page's WebSocket: JSON text both ways, beside the
//! binary video chunks of [`crate::video`].

use serde::{Deserialize, Serialize};

use crate::printer::{Printer, Sample};
use crate::video::VideoConfig;

/// What a signed-in person may do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Watches, and commands the printer.
    Operator,
    /// Watches only.
    Spectator,
}

impl Role {
    /// Whether this role may send commands to the printer.
    #[must_use]
    pub fn commands(self) -> bool {
        self == Self::Operator
    }

    /// The role as the page names it.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Operator => "opérateur",
            Self::Spectator => "spectateur",
        }
    }
}

/// What the server tells a page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant, reason = "a message is written out as soon as it is made, never kept in bulk")]
pub enum ServerMessage {
    /// First message on a socket: who is reading.
    Welcome { name: String, role: Role },
    /// The printer as it stands now; each one replaces the last.
    Printer(Printer),
    /// A new point of the temperature history, every few seconds.
    Sample(Sample),
    /// The video starts, or starts over: decode the chunks that follow with
    /// this configuration, from their first key chunk.
    Video(VideoConfig),
    /// The camera stopped sending, for the reason given.
    VideoLost { reason: String },
    /// What became of the command the page numbered `id`.
    Done { id: u32, outcome: Outcome },
}

/// What a page tells the server.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Start or stop sending this page the video.
    Watch { on: bool },
    /// Do `command`, and answer with [`ServerMessage::Done`] under `id`.
    Command { id: u32, command: Command },
}

/// What an operator can ask of the printer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum Command {
    /// Turn the chamber light on or off.
    Light { on: bool },
    /// Pause the print.
    Pause,
    /// Resume a paused print.
    Resume,
    /// Stop the print for good.
    Stop,
}

impl Command {
    /// The command as a sentence, for logs, confirmations and notifications.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Light { on: true } => "allumer la lumière",
            Self::Light { on: false } => "éteindre la lumière",
            Self::Pause => "mettre l'impression en pause",
            Self::Resume => "reprendre l'impression",
            Self::Stop => "arrêter l'impression",
        }
    }
}

/// What became of a command.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", content = "reason", rename_all = "snake_case")]
pub enum Outcome {
    /// The printer confirmed it.
    Done,
    /// It was refused or failed, for the reason given.
    Failed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_survive_json() {
        let sent = [
            ClientMessage::Watch { on: true },
            ClientMessage::Command { id: 7, command: Command::Light { on: false } },
            ClientMessage::Command { id: 8, command: Command::Stop },
        ];
        for message in sent {
            let text = serde_json::to_string(&message).unwrap();
            assert_eq!(serde_json::from_str::<ClientMessage>(&text).unwrap(), message);
        }
        let done = ServerMessage::Done { id: 3, outcome: Outcome::Failed("refusé".into()) };
        let text = serde_json::to_string(&done).unwrap();
        assert_eq!(serde_json::from_str::<ServerMessage>(&text).unwrap(), done);
    }

    #[test]
    fn only_operators_command() {
        assert!(Role::Operator.commands());
        assert!(!Role::Spectator.commands());
    }
}
