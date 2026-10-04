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

Existing CI and Release logs were reviewed on 2026-10-04; bounded, sanitized
summaries are preserved below so the evidence survives log/artifact expiration.
These results qualify only the recorded commits and commands. No hosted run was
returned for checkout commit `99542fee77382592ca1bbd2b388634505f2cc627` at review
time, and no checks were rerun for this documentation update. Complete current
platform/tool-version qualification remains pending.

| Gate | Available check | Recorded result for current changes |
| --- | --- | --- |
| Local docs, examples, Python and Rust checks | [Contributor checks](../CONTRIBUTING.md#checks) | Pending human-run checks |
| Linux/macOS integration | [CI matrix](../.github/workflows/ci.yml): Ubuntu 24.04 and ARM64 `macos-15` | [CI pass](#ci-pass-2026-10-04) at `4c35c43`; later [PR merge failed](#ci-failure-2026-10-04) before tests; current checkout pending |
| Coverage | CI Linux coverage job, 90% line threshold | [95.18% lines](#ci-pass-2026-10-04) at `4c35c43`; later PR merge failed compilation; current checkout pending |
| Real Herdr 0.9.1 | Isolated acceptance harness below | Unqualified; minimum-version gate open |
| Other Herdr versions, including 0.9.3 | Same harness with explicit version selection | No passing run recorded |
| Git 2.36.0 | Provider and lifecycle suites with that installed binary | No binary qualification recorded; parser fixtures alone are insufficient |
| Soak | 600-second fake-Herdr workload below | No resource/latency results recorded |
| Release artifacts | Native packaged-binary smoke checks in release workflow | [v0.2.0 built and published](#release-v020-2026-10-03); archives not smoke-tested by that run; current release qualification pending |
| Service managers | Native checks of customized examples below | Structure checks only are provided; activation not qualified |

For each completed gate, record the date, tested commit (and any uncommitted
changes), OS version, architecture, `rustc --version`, `cargo --version`,
`python3 --version`, `git --version`, and `herdr --version` when applicable.
Include the exact command, pass/fail result, and a CI run or sanitized artifact
reference. Record failures and skipped checks as such. A workflow definition or
installed tool version is not a passing result.

CI and both macOS Release entries use `macos-15` (ARM64). This selection was
reviewed on 2026-10-04 against GitHub's
[supported runner list](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)
and [macOS 14 retirement notice](https://github.blog/changelog/2026-10-01-github-actions-macos-14-runner-image-retirement/).
Versioned labels still receive software updates. Both workflows record the actual
OS version, architecture, and runner image version in their job summaries.
Release retains native `aarch64-apple-darwin` packaging smoke checks;
`x86_64-apple-darwin` is cross-built and is not executed on the ARM64 runner.

Deployment-minimum impact remains unverified pending hosted Release results.
This change sets no deployment-target override. Release records the selected SDK,
deployment-target environment setting, and binary Mach-O minimum OS/SDK metadata.
Before landing, compare that evidence for both triples with the previous release
and record any minimum-version change alongside the run URL and smoke results.
A newer SDK or build host does not establish compatibility with older macOS
versions; execution on those versions remains a separate qualification gate.

### CI pass: 2026-10-04

[Run 37190851976](https://github.com/dmytr0x/dmytr0x-herdr-tokens/actions/runs/37190851976),
09:02–09:04 UTC, **success**. All four checkout logs identify tested commit
`4c35c4331fd5d1d0998625defe0d55b60a680965`. Dirty state was not recorded before
checks; confirmation of uncommitted changes remains pending.

| Job (all passed) | Actual OS / host architecture | Runner image / version | Python |
| --- | --- | --- | --- |
| check (ubuntu-24.04) | Ubuntu 24.04.5 / x86-64 | ubuntu-24.04 / 20260927.320.1 | 3.12.14 |
| check (macos-15) | macOS 15.7.9 / ARM64 | macos-15-arm64 / 20260907.0337.1 | 3.12.10 |
| coverage | Ubuntu 24.04.5 / x86-64 | ubuntu-24.04 / 20260927.320.1 | 3.12.14 |
| musl | Ubuntu 22.04.5 / x86-64 | ubuntu-22.04 / 20260927.309.1 | Not recorded; pending |

All four jobs logged `rustc 1.94.0 (4a4ef493e 2026-03-02)` and Git 2.55.0.
Cargo's exact version was not recorded and remains pending. Herdr integration
uses test doubles, not a real-server acceptance run; no real Herdr version is
qualified. Git 2.55.0 execution does not qualify Git 2.36.0.

Both check jobs passed these exact commands (94 Rust tests, two doctests and
eight Python tests per platform):

```sh
for script in scripts/*.sh; do sh -n "$script" || exit; done
python3 -m unittest discover -s tests -p 'test_*.py'
python3 scripts/check-docs.py
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo test --locked --doc
cargo build --release --locked
```

Coverage used `cargo-llvm-cov 0.9.1` and passed the following command. The
reported total was 3,506 lines, 169 missed, **95.18% line coverage** (threshold
90%); region coverage was 93.13%, function coverage 93.80%.

```sh
set -o pipefail
cargo llvm-cov --locked --all-targets --summary-only --fail-under-lines 90 2>&1 | tee "$RUNNER_TEMP/coverage-summary.txt"
```

The musl job passed `cargo build --release --locked --target x86_64-unknown-linux-musl`;
it did not execute that binary. ShellCheck steps were skipped on macOS by design.
Neither a short soak nor the 600-second qualification soak ran in these jobs.

### CI failure: 2026-10-04

[Run 37190948652](https://github.com/dmytr0x/dmytr0x-herdr-tokens/actions/runs/37190948652),
09:04–09:05 UTC, **failure**. The run API reports PR head
`0d7e417a8433b7743420c6a3205262ab0df624d7`, but all four checkout logs identify
the actual tested merge commit `4663ce3eb98783664a3ecdeecff00ce3fd5e2699`
(merged into `4c35c4331fd5d1d0998625defe0d55b60a680965`). Dirty state was not
recorded before checks and remains pending. Each job logged the same OS,
architecture, runner image, Rust, Git and Python versions as its counterpart
in the passing run above; Cargo and musl-job Python versions remain pending.
No real Herdr acceptance or soak ran.

Both platform checks failed `cargo clippy --locked --all-targets -- -D warnings`
with exit 101: `E0277`, the SHA-256 digest type does not implement `LowerHex`,
at `src/config.rs:14:21` and `src/config.rs:358:25`. Shell syntax, Python tests,
documentation and formatting passed first. Rust tests, doctests and the later
native release builds did not run. macOS ShellCheck steps were skipped.

The musl build command and coverage command recorded above also failed with
exit 101 and the same compiler error. Coverage used `cargo-llvm-cov 0.9.1`;
no coverage percentage was produced. This failure does not replace the earlier
passing commit's result or establish a failure at the current checkout commit.

### Release v0.2.0: 2026-10-03

[Run 37128640595](https://github.com/dmytr0x/dmytr0x-herdr-tokens/actions/runs/37128640595),
14:09–14:11 UTC, **success**, at `dc377c0b8ad625817285e79126d7e5b6bf692735`.
Dirty state before checks/builds was not recorded and remains pending.
The source-test and four build jobs logged Rust 1.94.0
(`4a4ef493e 2026-03-02`) and Git 2.55.0; exact Cargo and Python versions for
those jobs remain pending. The separate tag-validation job set up Python
3.14.7. No real Herdr version was tested.

| Job / target | Actual OS / host architecture | Runner image / version | Result |
| --- | --- | --- | --- |
| Test release source | Ubuntu 24.04.5 / x86-64 | ubuntu-24.04 / 20260927.320.1 | Source tests passed |
| x86_64-unknown-linux-musl | Ubuntu 22.04.5 / x86-64 | ubuntu-22.04 / 20260927.309.1 | Built and packaged; archive not executed |
| aarch64-unknown-linux-musl | Ubuntu 24.04.5 / ARM64 | ubuntu-24.04-arm / 20260927.135.1 | Built and packaged; archive not executed |
| x86_64-apple-darwin | macOS 14.8.9 / ARM64 | macos-14-arm64 / 20260831.0302.1 | Cross-built and packaged; archive not executed |
| aarch64-apple-darwin | macOS 14.8.9 / ARM64 | macos-14-arm64 / 20260831.0302.1 | Built and packaged; archive not executed |

The source job passed `sh -n scripts/*.sh`, `cargo fmt --check`,
`cargo clippy --locked --all-targets -- -D warnings`, and `cargo test --locked`
(76 Rust tests; zero doctests). Each build passed the corresponding exact command:

```sh
cargo build --release --locked --target "x86_64-unknown-linux-musl"
cargo build --release --locked --target "aarch64-unknown-linux-musl"
cargo build --release --locked --target "x86_64-apple-darwin"
cargo build --release --locked --target "aarch64-apple-darwin"
```

All four archives and `SHA256SUMS` were published to the
[v0.2.0 release](https://github.com/dmytr0x/dmytr0x-herdr-tokens/releases/tag/v0.2.0).
Intermediate Actions artifacts had seven-day retention; this in-repository summary
and the release assets preserve the available evidence. That historical workflow
had no packaged-binary smoke step. Native archive execution, real-Herdr, old-Git,
soak and macOS deployment-minimum qualification remain open; successful packaging
does not satisfy them or qualify the later `macos-15` Release configuration.

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
