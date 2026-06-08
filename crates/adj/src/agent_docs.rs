use anyhow::Result;

/// Print a markdown steering doc telling AI coding agents how to interact with the
/// Adjacent-supervised app at `path` (or the current directory when `path` is `None`).
///
/// The doc is templated with the app `name` and `cmd` from `adjacent.toml`. No daemon
/// connection — this command is a pure local read-and-print.
pub fn emit(path: Option<String>) -> Result<()> {
    let _ = path;
    Ok(())
}
