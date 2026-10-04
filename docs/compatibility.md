# Compatibility and qualification

## Supported contract

The declared minimum Herdr version is **0.9.1**. This is a compatibility target,
not evidence that a real-server acceptance run passed on that version. The
[acceptance harness](../tests/real_herdr.py) defaults to 0.9.1 and requires an
explicit `--expected-herdr-version` for any other version; selecting a version
does not qualify it until the harness passes.

macOS and Linux are supported. The [release workflow](../.github/workflows/release.yml)
builds x86-64 and ARM64 binaries for each, using musl for Linux. A cross-build is
not native execution evidence. Windows and remote endpoint collection are not
supported. The source toolchain is pinned to Rust 1.94.0; development checks need
Python 3.11 or newer (`tomllib`), but installed emitters do not need Python or Cargo.

Git 2.36 or newer is required for Git collectors and background jobs. This is a
correction from the previous 2.20 minimum: worktree listing now requires NUL
porcelain output. Older newline output writes paths without escaping and cannot
represent every Unix filename unambiguously. No compatibility retry is performed
on a failed invocation. Spaces, quotes, tabs, newlines and non-UTF-8 path bytes are
preserved in NUL records. Command text output must be UTF-8 before ANSI removal.

Source evidence: [Git 2.20 worktree output](https://github.com/git/git/blob/v2.20.0/builtin/worktree.c)
and [Git 2.36 NUL output](https://github.com/git/git/blob/v2.36.0/builtin/worktree.c).
Parser fixtures cover normal and bare records, unusual bytes, and rejection of
legacy output. Fixture coverage does not establish execution against an older Git
binary. Command-only configurations without jobs do not require Git at runtime.
`validate` checks configuration syntax, schema and semantics without running
Git, Herdr, or configured commands; runtime preflight happens on `run`/`start`.

## Evidence and outstanding gates

No complete platform/tool-version qualification record for the current changes
is available in this checkout. The historical improvement-plan baseline reports
formatting, Clippy and 76 tests passing at `dc377c0` on 2026-10-03, but omits exact
OS, architecture and installed tool versions. It does not qualify the subsequent
changes or a particular Herdr/Git version. No checks were rerun for this
documentation update; the following gates remain pending recorded results.

| Gate | Available check | Recorded result for current changes |
| --- | --- | --- |
| Local docs, examples, Python and Rust checks | [Contributor checks](../CONTRIBUTING.md#checks) | Pending human-run checks |
| Linux/macOS integration | [CI matrix](../.github/workflows/ci.yml): Ubuntu 24.04 and `macos-latest` | No run URL/result recorded |
| Coverage | CI Linux coverage job, 90% line threshold | No measured result recorded |
| Real Herdr 0.9.1 | Isolated acceptance harness below | Unqualified; minimum-version gate open |
| Other Herdr versions, including 0.9.3 | Same harness with explicit version selection | No passing run recorded |
| Git 2.36.0 | Provider and lifecycle suites with that installed binary | No binary qualification recorded; parser fixtures alone are insufficient |
| Soak | 600-second fake-Herdr workload below | No resource/latency results recorded |
| Release artifacts | Native packaged-binary smoke checks in release workflow | No run results recorded; cross-built artifacts still require native checks |
| Service managers | Native checks of customized examples below | Structure checks only are provided; activation not qualified |

For each completed gate, record the date, tested commit (and any uncommitted
changes), OS version, architecture, `rustc --version`, `cargo --version`,
`python3 --version`, `git --version`, and `herdr --version` when applicable.
Include the exact command, pass/fail result, and a CI run or sanitized artifact
reference. Record failures and skipped checks as such. A workflow definition or
installed tool version is not a passing result.

## Qualification commands

Run from the repository root on an isolated test machine/session. Release,
coverage, soak, real-Herdr and old-Git gates are separate from the routine checks
in [CONTRIBUTING.md](../CONTRIBUTING.md).

```sh
cargo build --release --locked
python3 tests/real_herdr.py --expected-herdr-version 0.9.1
python3 tests/soak.py --seconds 600 --output /tmp/herdr-tokens-soak.json
```

The real-Herdr harness creates its own server, home, configuration and repositories,
loads the shipped sidebar example, and checks CLI contracts, manifest actions and
two-workspace behavior. Use `--herdr /absolute/path/to/herdr` to select a binary;
for 0.9.3, also use `--expected-herdr-version 0.9.3`. Do not use `--capture` during
qualification: it replaces checked-in fixtures. Passing on a newer Herdr does
not establish support for 0.9.1. Sidebar configuration acceptance does not verify
visual rendering.

The soak uses fake Herdr with 20 workspaces and four collectors, including two
failing collectors. It checks queue/concurrency bounds and sequence ordering and
records RSS and observed publication latency. Inspect the JSON for memory growth
and latency; successful completion alone is not a performance guarantee.

For the Git minimum-version gate, put an already installed Git 2.36.0 first on
`PATH` and run the following in that environment. Do not substitute a newer Git
result for this gate.

```sh
git --version
cargo test --locked --offline --test providers --test lifecycle
```

## Shipped example verification

`python3 scripts/check-docs.py` checks local Markdown link targets, parses shipped
TOML and service structures, and checks the demonstration shell script's syntax.
It does not start services or execute configured collectors.

| Example | Verification method and limit |
| --- | --- |
| [tokens.toml](../examples/tokens.toml) | `cargo test --locked --offline --test config shipped_examples_validate` loads the active example through production validation. Commented opt-in snippets must be enabled in a temporary config and checked with `herdr-tokens validate --config-dir PATH`; validation does not execute them. |
| [herdr-sidebar.toml](../examples/herdr-sidebar.toml) | TOML syntax via the docs checker; real Herdr config acceptance via the isolated harness. Visual styling needs manual inspection. |
| [ci-status](../examples/ci-status) | `sh -n examples/ci-status` via the docs checker; `sh examples/ci-status` should emit one JSON object with status `example only` and the demonstration URL. It does not query CI. |
| [systemd.service](../examples/systemd.service) | Docs checker checks foreground `run` and shutdown allowance. After replacing paths, use `systemd-analyze --user verify /absolute/path/to/herdr-tokens.service` on Linux. |
| [launchd.plist](../examples/launchd.plist) | Docs checker parses the plist and checks foreground `run` and shutdown allowance. After replacing paths, use `plutil -lint /absolute/path/to/local.herdr-tokens.plist` on macOS. |

For native service testing, use an isolated endpoint/config/state, customize every
placeholder path, and create the launchd log directory before loading the job.
Use the same runtime directory and normalized socket path for the service and
control CLI. Check readiness with `status --json`, stop through the service
manager, and confirm the runner exits. Record that result separately from syntax
validation. Service files are examples and are never installed automatically.
