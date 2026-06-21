use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod agent_docs;
mod client;
mod daemon;
mod doctor;
mod env;
mod installca;
mod paths;
mod portforward;
mod proxy;
mod readiness;
mod registry;
mod scaffold;
mod status;
mod supervisor;
mod tls;
mod worktree;

/// Build-stamped version: `ADJ_VERSION` (the CI tag or `git describe`, set by build.rs) when
/// present, else the crate version from Cargo.toml.
const VERSION: &str = match option_env!("ADJ_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

#[derive(Parser)]
#[command(
    name = "adj",
    version = VERSION,
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
    Add {
        path: String,
        /// Register as a named instance: `<label>.<name>.adj.ac`. Defaults to the sanitized
        /// git branch name when the directory is a linked git worktree.
        #[arg(long)]
        label: Option<String>,
    },
    /// List registered apps and their state.
    List {
        /// Emit a JSON array of `{name, path, state, port?, stale?}` instead of the human view.
        #[arg(long)]
        json: bool,
    },
    /// Boot a registered app.
    Up { name: String },
    /// Stop a running app (SIGTERM, then SIGKILL after a grace period).
    Down { name: String },
    /// Restart an app (down then up).
    Restart { name: String },
    /// Remove an app from the registry (stopping it first if running).
    Remove { name: String },
    /// Remove every registry entry whose directory no longer exists on disk.
    Prune,
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
    /// Block until an app reports ready (TCP-open or 2xx from health_check_url).
    WaitReady {
        name: String,
        /// Override the per-app boot_timeout in seconds. `0` (the default) means use the app's
        /// configured `boot_timeout`.
        #[arg(long, default_value_t = 0)]
        timeout: u64,
    },
    /// Print a markdown steering doc telling AI coding agents how to interact with
    /// the Adjacent-supervised app in the target directory.
    AgentInstructions {
        /// Directory containing `adjacent.toml`. Defaults to the current directory.
        #[arg(long)]
        path: Option<String>,
    },
    /// Print the pf anchor and the sudo command to redirect :80 to the proxy port.
    InstallPortForward,
    /// Generate the local HTTPS CA (if missing) and print the sudo command to trust it.
    InstallCa {
        /// Wipe the Secure-Enclave-backed CA key and the on-disk cert. Use to start fresh, or as
        /// test teardown. Prints the untrust command but does not run it — the trust anchor in
        /// the system keychain is yours to remove.
        #[arg(long)]
        reset: bool,
    },
    /// Verify the local install end-to-end: pf port-forward rule, daemon reachability, and the
    /// local CA (on-disk cert, keychain key, signing ACL, system trust). All checks are rootless.
    /// Exit status is 0 when everything passes, 2 when any check fails.
    Doctor,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    let result = match cli.cmd {
        Cmd::Daemon => daemon::run().await,
        Cmd::Add { path, label } => client::add(path, label).await,
        Cmd::List { json } => client::list(json).await,
        Cmd::Up { name } => client::up(name).await,
        Cmd::Down { name } => client::down(name).await,
        Cmd::Restart { name } => client::restart(name).await,
        Cmd::Remove { name } => client::remove(name).await,
        Cmd::Prune => client::prune().await,
        Cmd::Status { name, json } => client::status(name, json).await,
        Cmd::Logs { name, tail, json } => client::logs(name, tail, json).await,
        Cmd::WaitReady { name, timeout } => client::wait_ready(name, timeout).await,
        Cmd::AgentInstructions { path } => agent_docs::emit(path),
        Cmd::InstallPortForward => portforward::install(),
        Cmd::InstallCa { reset } => {
            if reset {
                installca::reset()
            } else {
                installca::install()
            }
        }
        Cmd::Doctor => doctor::run(),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // `{err:#}` uses anyhow's alternate format, which prints the full cause chain
            // separated by ": ". Without `:#`, only the outermost `.context(...)` shows up and
            // genuinely-helpful inner errors are lost.
            eprintln!("adj: {err:#}");
            ExitCode::from(1)
        }
    }
}
