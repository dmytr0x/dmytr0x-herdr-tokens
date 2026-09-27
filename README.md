# Herdr Tokens Emitter

One Rust executable that collects **display-only** workspace metadata and reports it to Herdr. Herdr owns sidebar rendering, storage and TTL expiry. macOS and Linux; Herdr **0.9.1** is the verified contract. Later versions require contract testing.

## Install and start

Prerequisites: Cargo/rustup (the checkout pins Rust 1.94.0), Herdr ≥0.9.1 and Git ≥2.20 when using the Git provider. Local linking does **not** build the plugin.

```sh
cargo build --release --locked
herdr plugin link "$PWD"
CONFIG_DIR="$(herdr plugin config-dir herdr-tokens)"
# Do not overwrite an existing configuration:
test -e "$CONFIG_DIR/tokens.toml" || cp examples/tokens.toml "$CONFIG_DIR/tokens.toml"
./target/release/herdr-tokens validate --config-dir "$CONFIG_DIR"
herdr plugin action invoke herdr-tokens.start
```

### Release binaries

Tagged releases publish prebuilt `herdr-tokens` archives for Apple Silicon and Intel macOS, and static musl binaries for x86-64 and ARM64 Linux. Every release also includes `SHA256SUMS`. These assets prepare the plugin for binary installation, but the current manifest does not download them yet: installation from a checkout still runs Cargo as shown above.

Add rows to **Herdr's own** configuration, not `tokens.toml`:

```toml
[ui.sidebar.spaces]
rows = [
  ["workspace", "branch", "git_status"],
  [
    { token = "$git_modified", fg = "#f9e2af" },
    { token = "$git_staged", fg = "#a6e3a1" },
    { token = "$git_untracked", fg = "#89b4fa" },
    { token = "$git_conflicts", fg = "#f38ba8" },
  ],
]
```

Use `herdr server reload-config` to apply sidebar edits. Linking, enabling and reloading Herdr do **not** launch plugin startup hooks. Start manually as shown above. Hooks run after a server starts; they are not supervisors.

```sh
herdr plugin action invoke herdr-tokens.status
herdr plugin action invoke herdr-tokens.refresh
herdr plugin action invoke herdr-tokens.reload
herdr plugin action invoke herdr-tokens.stop
```

Herdr action invocation is asynchronous: inspect its command log (`herdr plugin log list`) for output, or use the executable's `status` command directly.

**Stop before disable, uninstall or update.** Disable/uninstall does not kill an independently detached runner. To update: stop, wait until `status` reports no runner, update/build with `--locked`, then start. Do not replace a running binary and assume its process changed. A detached runner waits through server outages until stopped.

## CLI and locations

```text
herdr-tokens run
herdr-tokens start
herdr-tokens stop
herdr-tokens validate
herdr-tokens reload
herdr-tokens refresh [--workspace ID]
herdr-tokens status [--json] [--include-values]
```

| Option | Default |
|---|---|
| `--config-dir PATH` | `HERDR_PLUGIN_CONFIG_DIR` |
| `--state-dir PATH` | `HERDR_PLUGIN_STATE_DIR` |
| `--socket PATH` | `HERDR_SOCKET_PATH`; **no implicit default session** |
| `--herdr-bin PATH` | `HERDR_BIN_PATH`, then `herdr` resolved on `PATH` |
| `--runtime-dir PATH` | `/tmp/herdr-tokens-<uid>` |

Options work before or after the subcommand. Relative explicit paths resolve against the invocation directory. `validate` needs only config; control commands need only endpoint/runtime identity. `run`/`start` additionally need config and durable state. For example:

```sh
./target/release/herdr-tokens run \
  --socket /absolute/path/to/herdr.sock \
  --config-dir /absolute/path/to/plugin-config \
  --state-dir /absolute/path/to/plugin-state
```

`run` stays in the foreground and logs to stderr. `start` detaches the same executable and waits up to five seconds for readiness; Herdr can be offline at readiness. Both are idempotent for a matching endpoint/config/state. Different config/state paths conflict. All launch paths for an endpoint must use the **same runtime directory and normalized socket path**. Socket aliases are unsupported.

`refresh` acknowledges scheduling, not collection success. A plain CLI invocation refreshes all workspaces unless `--workspace` is supplied; an incidental terminal `HERDR_WORKSPACE_ID` is ignored. Only the manifest refresh action uses injected workspace context. `reload` acknowledges a validated commit, not completion of all pending clears.

Exit codes: **0** success, **1** runtime/control failure, **2** invalid arguments/configuration. `status --json` returns a versioned response with `result`, `identity`, `ready` and `ok`. Values are hidden unless `--include-values` is explicit. Status is the emitter's knowledge, **not proof of what Herdr currently displays**.

## Configuration

`tokens.toml` is required. Direct regular `tokens.d/*.toml` fragments are appended in filename-byte order. Fragments may only contain collectors. No overrides, includes, symlinks or repository-local configuration discovery. Maximum: 64 files / 1 MiB total.

```toml
schema_version = 1

[runtime]
max_concurrency = 4
discovery_interval_ms = 5000

# Optional, session-specific. Must be an absolute local directory.
# [workspace_dirs]
# w1 = "/absolute/project"

[[collectors]]
name = "ci"
provider = "command"
command = ["./scripts/ci-status"]
interval_ms = 10000
timeout_ms = 2000
ttl_ms = 30000
# Explicit opt-in for additional inherited variables:
env_allow = ["CI_TOKEN"]
[collectors.env]
CI_FORMAT = "json"
[collectors.tokens]
ci_status = "status"
ci_url = "url"
```

By default, the script must emit exactly one JSON object, for example:

```json
{"status":"passing","url":"https://ci.example.com/build/123"}
```

For a command whose stdout is already the complete value, select explicit text output instead. No shell or JSON adapter is needed:

```toml
[[collectors]]
name = "date"
provider = "command"
global = true
command = ["date", "+%Y-%m-%d %H:%M:%S"]
output = "text"
interval_ms = 10000
timeout_ms = 2000
ttl_ms = 30000
[collectors.tokens]
current_date = "stdout"
```

Text output only supports the literal `stdout` selector. It must be UTF-8; surrounding whitespace and ANSI escape sequences are removed, normalized empty output clears the token, and the result uses the same 80-character limit as JSON values. `output` defaults to `json`, preserving existing configurations. Do not use automatic JSON/text detection: malformed JSON remains an observable collection failure.

Command collectors with `global = true` run once per interval for the connected Herdr session and fan the same patch out to every discovered workspace, including workspaces without a resolved directory. Their working directory is the plugin configuration directory. They receive `HERDR_TOKENS_COLLECTOR`, but not `HERDR_TOKENS_WORKSPACE_ID` or `HERDR_TOKENS_WORKSPACE_DIR`. A newly discovered workspace receives the latest sufficiently fresh cached result without rerunning the command. Any refresh schedules one global execution and fan-out; a workspace filter still only limits workspace-scoped collectors. Global Git collectors are invalid. `global` defaults to `false`.

Workspace-scoped collectors apply to **every eligible workspace**. Explicit mappings are required. A mapping is either a field-name string or `{ field = "name", prefix = "…", suffix = "…" }`. JSON selectors are literal top-level property names (including dots), not JSONPath. Strings/numbers/booleans become text; `null` or normalized empty strings explicitly clear. Missing selected fields, selected arrays/objects, duplicate top-level JSON keys, invalid UTF-8, trailing output, nonzero exits, timeouts or overflow fail the entire collection. Unmapped fields are ignored.

Strings are trimmed, stripped of Unicode controls, truncated to 80 Unicode scalar values, then trimmed again. Truncation appears in diagnostics. URLs are display text, not promised clickable links or complete URL storage.

| Field | Default | Allowed |
|---|---|---|
| `schema_version` | required | `1` |
| `output` | `json` | command provider: `json` or `text` |
| `global` | `false` | command provider: boolean |
| `max_concurrency` | 4 | 1–64 |
| `discovery_interval_ms` | 5000 | 1000–60000 |
| `interval_ms` | 10000 | 250–28800000 |
| `timeout_ms` | min(1000, interval) | 1–min(interval, 300000) |
| `ttl_ms` | 3 × interval | 3 × interval through 86400000 |
| `env_allow` / `env` | empty | command provider only |

Names use ASCII letters, digits, `_`, `-`: collector names 1–64 characters; token names 1–32, **without `$`**. Names and tokens must be unique across the complete configuration. Each collector maps 1–16 tokens; total ≤32. Selectors are nonempty and ≤128 UTF-8 bytes. Unknown fields and invalid types are errors. Empty collectors (`schema_version = 1` alone) is a valid explicit disable-and-clear configuration.

The Git provider uses installed Git with porcelain-v2 NUL-delimited output, optional locks and fsmonitor disabled, and submodules ignored. Fields:

- `modified_files`: ordinary/rename entries changed in the worktree.
- `staged_files`: entries changed in the index. A file can count in both.
- `untracked_files`: files, not collapsed directories.
- `conflict_files`: unmerged entries, counted only here.

Zero is a real value. A non-repository or unsafe/unavailable checkout fails; it does not fabricate zeros. Detached HEAD, linked worktrees and absent upstream work normally. These are **file counts, not diff line counts**. Git collectors reject `command`, `env` and `env_allow`.

### Customize token appearance

In the plugin's `tokens.toml`, use literal prefixes and suffixes per token:

```toml
[collectors.tokens]
git_modified = { field = "modified_files", prefix = "[!", suffix = "]" }
git_staged = { field = "staged_files", prefix = "✓" }
git_untracked = { field = "untracked_files", prefix = "?" }
git_conflicts = { field = "conflict_files", prefix = "✖" }
```

For counts `2, 1, 3, 0`, these publish `[!2]`, `✓1`, `?3`, `✖0`. Existing mappings such as `git_modified = "modified_files"` still publish the plain number. Both providers support the same decoration. Prefix/suffix default to empty, each accept at most 80 Unicode characters, and reject control characters. They are literal text, not templates, shell commands or ANSI escapes.

Values are normalized before decoration and the complete decorated value is normalized again to Herdr's **80-character total limit**; long text can truncate a suffix. Null/normalized empty output still clears the token—no prefix-only placeholders. Zero is shown by default and false remains a real value. Changing decoration or visibility participates in normal transactional reload and clear barriers; switching between equivalent shorthand/longhand does not restart jobs.

To reduce clutter, set `show_zero = false` on individual mappings (both providers):

```toml
[collectors.tokens]
git_modified = { field = "modified_files", prefix = "[!", suffix = "]", show_zero = false }
git_conflicts = { field = "conflict_files", prefix = "✖", show_zero = false }
```

Normalized `0` and `0.0` (numbers or strings) then explicitly clear the entire token, including its prefix and suffix, so previously displayed values disappear too. Null, empty and whitespace-only values always clear, regardless of this flag. `show_zero` defaults to `true` for backward compatibility; `show_always` is accepted as an alias. Keep any text that should disappear in the mapping's prefix/suffix, not as literal text in Herdr's sidebar template.

**Colors belong in Herdr's own sidebar configuration**, as in the installation example above and [`examples/herdr-sidebar.toml`](examples/herdr-sidebar.toml). Herdr 0.9.1 supports per-occurrence `fg` (`#RGB` / `#RRGGBB`), `bold` and `dim`. Foreground color applies to the entire decorated token, including its prefix/suffix. Named palette references such as `fg:yellow` and a collector `format` string are not supported by this plugin/transport.

**Per-token background colors are not supported by Herdr 0.9.1.** To approximate a shared `bg:base`, configure the sidebar and active-row backgrounds in Herdr:

```toml
[theme.custom]
sidebar_bg = "#1e1e2e"
active_row_bg = "#1e1e2e"
```

These affect the sidebar/active row globally, not individual token spans. Herdr also controls row ordering and the ` · ` separators; the plugin cannot replace those with a custom composite layout. Styling rules match the **decorated** reported text: e.g. hide zero staged files with `rules = [{ equals = "✓0", hide = true }]`, not a numeric comparison against the raw count. Keep a plain mapping if you need numeric rules.

### Directories and trust

For workspace-scoped collectors, directory priority is: explicit override → worktree `checkout_path` → agreement among valid pane `cwd` directories. Never use `foreground_cwd`, a guessed workspace cwd or the emitter's own directory. Missing/ambiguous directories are ineligible. An unavailable override/worktree never falls back. Directory changes fence old workspace-scoped work and clear its tokens before replacements. Global commands instead use the plugin configuration directory and do not depend on workspace directory discovery.

Commands run without an implicit shell, with null stdin, in supervised Unix process groups. A shell requires explicit argv such as `["sh", "-c", "..."]`. Absolute executables are used directly, slash-containing relative executables resolve against the command's working directory, and bare names use sanitized `PATH`.

Inherited environment defaults: `PATH`, `HOME`, `USER`, `LOGNAME`, `TMPDIR`, `TMP`, `TEMP`, `LANG`, `LC_ALL`; missing PATH defaults to `/usr/bin:/bin`. Then command `env_allow`, explicit `env`, and plugin-controlled `HERDR_TOKENS_COLLECTOR`. Workspace-scoped commands additionally receive `HERDR_TOKENS_WORKSPACE_ID` and `HERDR_TOKENS_WORKSPACE_DIR`; global commands do not. Those context variables cannot be overridden. Collector secrets/socket/pane/Git overrides are not inherited by default. Herdr CLI children use a separate environment.

**This is not a sandbox.** Repository-relative scripts are repository-controlled code with your permissions. Only configure scripts for repositories you trust. Allow-lists cannot stop a program from reading your files. Do not put secrets in tokens. Logs omit values, argv, environment values and raw child stderr; status reveals values only on request.

### Reloads, freshness and ownership

Content is polled each second and a changed snapshot confirmed after 200 ms. Invalid candidates preserve the complete active configuration. Valid changes cancel old work, advance generations and clear tokens of removed/changed collectors before replacement publication. Identical normalized configurations do not restart jobs. Multi-file edits are not atomic: use one file for inseparable changes and write through atomic replacement. Removing the required main file is invalid, not a disable operation.

Successful collections refresh TTL even when values are unchanged. Failures do not publish fake error values or refresh old TTLs. Queued results older than their collector interval are discarded; TTL is reduced by queue age. Under overload/outage, values can expire despite the 3× interval minimum. Missed deadlines are observable, never hidden by extending TTL.

Herdr has a shared 32-key workspace map, not source-owned namespaces. **Reserve exclusive token names for this plugin.** Another reporter can overwrite or be cleared by it; the plugin cannot detect ownership. It never clears arbitrary unconfigured keys to recover capacity. Normal collector reports contain at most 16 keys. Larger reconciliation clears are chunked, serialized and fenced with barriers.

## Runtime state and recovery

Private runtime entries use mode 0700 directories and 0600 lock/socket files, owned by the current user. Control is same-user IPC, not protection against malicious programs already running as you. Durable files:

```text
STATE_DIR/endpoints/<sha256-of-normalized-socket>/sequence.toml
STATE_DIR/endpoints/<sha256-of-normalized-socket>/logs/day-<UTC-epoch-day>.jsonl
```

Herdr's existing state root may be 0755, but must not be group/world writable; managed descendants are 0700. An endpoint-global advisory lock is independent of the state directory. One stable metadata source, `herdr-tokens`, shares a durable block-reserved sequence allocator across every collector and workspace. Reservations are atomically persisted and fsynced before use. Restarts skip unused numbers. Corrupt/unwritable/exhausted sequence state is fatal.

**Never delete or restore old sequence state while the Herdr server retains metadata history.** Herdr silently ignores reused sequences and does not expose its watermark. Recovery: stop the runner; restart the affected Herdr server (resetting metadata history); only then initialize fresh sequence state. Ordinary upgrades/restarts must preserve state. No-longer-configured tokens after an emitter restart expire by their old TTL; past ownership is not reconstructed.

Detached logs retain at most seven daily files with a 10 MiB/day cap and suppression marker. Foreground logs go to stderr. Repeated failures are rate-limited. On stop/SIGINT/SIGTERM, child groups are cancelled/reaped and clears attempted within a five-second shutdown budget. Crashes and unavailable endpoints rely on finite TTL. Escaped malicious process groups are outside the cleanup threat model.

For service-manager supervision, see `examples/systemd.service` and `examples/launchd.plist`. Customize explicit paths; nothing installs these automatically. Use `run`, not `start`.

## Development and qualification

See [CONTRIBUTING.md](CONTRIBUTING.md) for contribution guidelines and coverage checks, and [SECURITY.md](SECURITY.md) for reporting sensitive findings. Licensed under [MIT](LICENSE).

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
python3 tests/real_herdr.py --capture   # optional; isolated server/config/home/repos
python3 tests/soak.py --seconds 600 --output /tmp/herdr-tokens-soak.json
```

Python 3 is a **test-only** prerequisite for the fake CLI/lifecycle and acceptance harnesses. Tests never target your active session. CI runs formatting, Clippy and tests on Linux and macOS. See [compatibility and release qualification](docs/compatibility.md) for observed results and remaining gates. Automatic consumption of release binaries during plugin installation, Windows, SSH collection, HTTP/file providers, watchers and dynamic provider registries are deferred. Run a separate emitter on a remote host against that host's local endpoint.
