//! `x2d`: the server of a Bambu Lab printer's live page, and the commands
//! that run it and invite its people.

mod bot;
mod camera;
mod check;
mod commands;
mod config;
mod ftps;
mod h264;
mod history;
mod hub;
mod mqtt;
mod notify;
mod people;
mod report;
mod rtsp;
mod session;
mod summary;
mod telegram;
mod texts;
mod thumbnail;
mod tls;
mod web;
mod webapp;

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use x2d_protocol::Role;

use crate::config::Config;
use crate::people::People;

/// The live page of a Bambu Lab printer.
#[derive(Parser)]
#[command(version)]
struct Arguments {
    /// The configuration file.
    #[arg(long, env = "X2D_CONFIG")]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the page.
    Serve,
    /// Reach the printer and its camera once, and tell what they say.
    Check {
        /// The directory to write the pictures of the camera and of the
        /// plate printed in.
        #[arg(long)]
        pictures: Option<PathBuf>,
    },
    /// Invite someone, or invite them again, and print their one-time link.
    Invite {
        /// Their name, as the page greets them.
        name: String,
        /// What they may do: if not given, what they could before, or
        /// spectateur for someone new.
        #[arg(long, value_enum)]
        role: Option<Kind>,
        /// How many days the link can be accepted for.
        #[arg(long, default_value_t = 7)]
        days: u32,
    },
    /// Forget someone: their link and every browser they signed in on.
    Revoke {
        /// Their name.
        name: String,
    },
    /// List the people invited.
    People,
}

/// What an invited person may do.
#[derive(Clone, Copy, ValueEnum)]
enum Kind {
    /// Watches, and commands the printer.
    #[value(alias = "operator")]
    Operateur,
    /// Watches only.
    #[value(alias = "spectator")]
    Spectateur,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "x2d=info,warn".into()),
        )
        .init();
    tls::install_default_provider();
    let arguments = Arguments::parse();
    let config = Config::read(&arguments.config)?;
    let people = People::new(&config.data);
    match arguments.command {
        Command::Serve => serve(config).await,
        Command::Check { pictures } => check::run(&config, pictures.as_deref()).await,
        Command::Invite { name, role, days } => {
            let role = role.map(|role| match role {
                Kind::Operateur => Role::Operator,
                Kind::Spectateur => Role::Spectator,
            });
            std::fs::create_dir_all(&config.data)?;
            let (token, _) = people.invite(&name, role, days)?;
            println!("{}/invite/{token}", config.origin);
            Ok(())
        }
        Command::Revoke { name } => {
            if people.revoke(&name)? {
                println!("{name} is revoked.");
                Ok(())
            } else {
                anyhow::bail!("nobody is called {name}")
            }
        }
        Command::People => {
            for person in people.list()? {
                let pending = person
                    .invited_until
                    .map_or_else(String::new, |until| format!(", invited until {until} (Unix time)"));
                println!("{} ({}): {} login(s){pending}", person.name, person.role.label(), person.logins);
            }
            Ok(())
        }
    }
}

/// Serve the page until the process is asked to stop.
async fn serve(config: Config) -> anyhow::Result<()> {
    let listen = config.listen;
    let hub = hub::Hub::start(config)?;
    let listener = tokio::net::TcpListener::bind(listen).await?;
    tracing::info!("serving on {listen}");
    axum::serve(listener, web::router(hub))
        .with_graceful_shutdown(async {
            let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = async { if let Some(terminate) = &mut terminate { terminate.recv().await; } } => {}
            }
        })
        .await?;
    Ok(())
}

/// The time now, in Unix seconds.
pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs() as i64)
}
