# Contributing

Use macOS or Linux with the Rust toolchain pinned in `rust-toolchain.toml`, Git, and Python 3. Herdr is only required for the optional real-server acceptance harness.

## Checks

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo build --release --locked
```

For coverage:

```sh
rustup component add llvm-tools-preview
cargo install cargo-llvm-cov --version 0.9.1 --locked
cargo llvm-cov --locked --all-targets --summary-only --fail-under-lines 90
```

The coverage run includes unit, integration, and executable lifecycle tests. Keep `LLVM_PROFILE_FILE` forwarding in the lifecycle harness: otherwise child-process execution disappears from the report. Do not extend the production collector environment for instrumentation.

## Changes

- Keep changes focused and add regression tests for changed behavior.
- Test pure rules directly; test process, filesystem, and IPC behavior through real isolated resources. Use Tokio's paused clock for time-based rules where possible.
- Keep scheduling and publication decisions in the coordinator; provider tasks return results rather than mutating shared state.
- Prefer purpose-driven names and small interfaces over generic frameworks. Comments should explain constraints or safety decisions, not restate code.
- Preserve configuration validation, redaction, cancellation cleanup, generation fencing, and finite TTL behavior.
- Update user-facing documentation when behavior changes. Do not commit generated output, credentials, personal paths, or unsanitized captures.

Before opening a pull request, include the motivation, relevant tests, and any compatibility impact. Bug reports should include OS/tool versions, reproduction steps, and a minimal sanitized configuration. Avoid `status --include-values` output unless every value is safe to share.

See [compatibility and qualification](docs/compatibility.md) for acceptance commands and the remaining release gates. Passing coverage is not proof of cross-platform compatibility or crash safety.

## First GitHub publication

- Confirm the existing MIT license and copyright attribution are appropriate.
- Choose the repository owner/name and add its actual URL as `package.repository` in `Cargo.toml`.
- Initialize Git, inspect the complete staged file list, and create the remote only after reviewing the files to be published.
- Enable GitHub private vulnerability reporting and branch protection requiring the CI checks.
- Run the Linux/macOS CI matrix before tagging a release; keep unresolved qualification limits visible.
