use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod client;
mod daemon;
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
    List {
        /// Emit a JSON array of `{name, path, state, port?}` instead of the human view.
        #[arg(long)]
        json: bool,
    },
    /// Boot a registered app.
    Up { name: String },
    /// Stop a running app (SIGTERM, then SIGKILL after a grace period).
    Down { name: String },
    /// Restart an app (down then up).
    Restart { name: String },
    /// Report the current state of an app.
    Status {
        name: String,
        /// Emit a JSON object of `{name, path, state, port?, pid?, exit_code?, started_at?}`.
        #[arg(long)]
        json: bool,
    },
    /// Print the log file for an app.
    Logs {
        name: String,
        /// Stream new log lines as they arrive (tail -F equivalent).
        #[arg(long)]
        tail: bool,
        /// Emit one JSON object per line (`{ts, stream, line}`) instead of plain text.
        #[arg(long)]
        json: bool,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    let result = match cli.cmd {
        Cmd::Daemon => daemon::run().await,
        Cmd::Add { path } => client::add(path).await,
        Cmd::List { json } => client::list(json).await,
        Cmd::Up { name } => client::up(name).await,
        Cmd::Down { name } => client::down(name).await,
        Cmd::Restart { name } => client::restart(name).await,
        Cmd::Status { name, json } => client::status(name, json).await,
        Cmd::Logs { name, tail, json } => client::logs(name, tail, json).await,
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("adj: {err}");
            ExitCode::from(1)
        }
    }
}
