# Contributing

Use macOS or Linux with the Rust toolchain pinned in `rust-toolchain.toml`, Git ≥2.36, and Python ≥3.11. Herdr is only required for the optional real-server acceptance harness.

## Checks

```sh
python3 -m unittest discover -s tests -p 'test_*.py'
python3 scripts/check-docs.py
for script in scripts/*.sh; do sh -n "$script" || exit; done
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo test --locked --doc
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

That document also lists a verification method for every shipped example and the
evidence required to close a qualification gate. Record actual platform/tool
versions and results; a configured CI job is not qualification evidence.

## Releases

Keep the versions in `Cargo.toml` and `herdr-plugin.toml` identical. After the commit passes CI, create and push the matching tag:

```sh
git tag v0.2.0
git push origin v0.2.0
```

The release workflow rejects a tag that is not exactly `v<manifest-version>`. It builds archives for macOS and Linux on x86-64 and ARM64, generates `SHA256SUMS`, and creates the GitHub Release. Re-running the workflow downloads existing assets and requires byte-for-byte matches. Missing or different published assets fail without replacement. Rebuilds are not assumed reproducible; use a new version for different bytes. Do not move a published version tag to different source; publish a new version instead.

Release builds smoke-test packaged executables on matching host architectures and
write the result (or explicit non-execution) to the workflow summary. Cross-built
macOS artifacts still need native qualification before release. No workflow
pushes commits or moves tags on a rerun.

Actions remain version-tagged. Review upstream release notes and permissions when
updating an action major, update CI and release workflows together, and run both
platform checks. Immutable action revisions and automated dependency updates are
a separate maintenance change; no unmaintained pins are introduced here.

Use the real-Herdr harness without `--capture` for qualification. Captured fixture
replacement is a separate reviewed change. Select `--expected-herdr-version`
explicitly when qualifying a version other than 0.9.1, and record the actual result
in the compatibility document. Never point the harness at an active user endpoint.
