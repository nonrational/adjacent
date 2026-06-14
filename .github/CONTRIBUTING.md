# Contributing to Adjacent

Adjacent is a local dev-server harness built to let human developers and agent developers share one supervised server.

## Scope

Currently, the project is scoped explicitly to macOS (Apple Silicon). Support for other platforms isn't a near-term goal.

## Local Development

The toolchain is pinned via `asdf` — see `.tool-versions` for the Rust and Node.js versions. Install the `asdf` plugins, then run `asdf install` from the repo root.

```sh
cargo build  # workspace build, binary at target/debug/adj
cargo test   # unit + integration tests
```

## Pull Requests

All PRs must pass the same CI gates:

- `cargo fmt --check`
- `cargo clippy -- -D warnings`
- `cargo test`

### Conventions

- PR bodies should use `Resolves #N` instead of `Closes #N`.
- We do not use Conventional Commit prefixes (e.g., `fix:`, `feat:`). Use plain, descriptive commit messages.

## License

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this work by you, as defined in the Apache-2.0 license, shall be dual licensed under the Apache License, Version 2.0 and the MIT license, without any additional terms or conditions.
