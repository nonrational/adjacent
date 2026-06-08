use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod client;
mod daemon;
mod env;
mod paths;
mod registry;
mod supervisor;

#[derive(Parser)]
#[command(
    name = "adj",
    version,
    about = "Adjacent: supervised local dev servers"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the Adjacent daemon in the foreground.
    Daemon,
    /// Register an app from a directory containing adjacent.toml.
    Add { path: String },
    /// List registered apps and their state.
    List,
    /// Boot a registered app.
    Up { name: String },
    /// Stop a running app (SIGTERM, then SIGKILL after a grace period).
    Down { name: String },
    /// Restart an app (down then up).
    Restart { name: String },
    /// Report the current state of an app.
    Status { name: String },
    /// Print the log file for an app.
    Logs {
        name: String,
        /// Stream new log lines as they arrive (tail -F equivalent).
        #[arg(long)]
        tail: bool,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    let result = match cli.cmd {
        Cmd::Daemon => daemon::run().await,
        Cmd::Add { path } => client::add(path).await,
        Cmd::List => client::list().await,
        Cmd::Up { name } => client::up(name).await,
        Cmd::Down { name } => client::down(name).await,
        Cmd::Restart { name } => client::restart(name).await,
        Cmd::Status { name } => client::status(name).await,
        Cmd::Logs { name, tail } => client::logs(name, tail).await,
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("adj: {err}");
            ExitCode::from(1)
        }
    }
}
