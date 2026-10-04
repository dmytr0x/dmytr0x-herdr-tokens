# Runtime invariants

The coordinator is the single owner of mutable scheduling, discovery, publication,
and diagnostic state. Spawned tasks return outcomes. Only the coordinator commits
configuration, connection, and workspace directory generations.

- `config` converts the unchanged TOML schema into validated provider variants.
  Command scope is part of the command variant; Git cannot be global. Command
  collectors and background jobs share command/environment settings, with separate
  scheduling rules. Diagnostics identify main file or numbered fragment, entry,
  and byte range without printing parser excerpts or configured values.
- `runner/planning` decides workspace differences, configuration candidate
  stability, and reload scope. Adding or moving a workspace cancels only its
  affected collectors. Unchanged tasks retain schedules and refresh requests.
  Job-only reloads preserve collector generations and publications.
- `task::OwnedTask` couples process-owning join handles with cancellation. Normal
  completion, reload, disconnect, and shutdown join the owned tasks. Dropping an
  owner cancels its task and schedules its join; process-group guards kill groups
  if a supervisor future is aborted. Explicit async reaping remains the normal
  path. IPC-only control tasks can be aborted after their acknowledgement grace.
- `process` bounds output and execution. Shutdown signals active work together,
  joins it, then attempts clears while the shared five-second budget permits a
  full transport operation. Cancellation does not drop cleanup futures. The
  termination grace is 100 ms; a transport call has a one-second deadline. OS
  reaping/scheduling can exceed wall-clock budgets. Descendants that deliberately
  escape the group remain outside the supported cleanup model.
- `publisher` owns clear barriers, freshness, and fair pending publication. A send
  with unknown completion may still have reached Herdr. The barrier therefore
  includes TTL plus the shared transport deadline and termination grace. A clear
  acknowledgement or expiry of that conservative bound releases the barrier.
  Generation validation is shared by dispatch and completion. A global collection
  uses directory generation zero; each fan-out publication uses its workspace's
  current directory generation and still obeys the cached result's freshness.
- `runtime/sequences` borrows the endpoint lock exclusively. Rust prevents a second
  live allocator under that lock or releasing the lock while it is in use. A
  reservation replaces the sequence file, fsyncs the file and parent directory,
  then advances memory. Failure is fatal to the runner; restarting skips the last
  reserved block. Never reset production sequence state while Herdr retains its
  sequence history.
- `runtime/protocol` decodes the existing newline-delimited version-1 wire request
  into typed commands. Irrelevant optional fields remain ignored for compatibility;
  unknown commands and invalid versions/endpoint identities return stable errors.
  Request and response frames have separate bounds (8 KiB and 32 MiB) and retain
  five-second deadlines. Status structs describe local observations. Omitted
  values stay omitted unless explicitly requested; null observations stay null.

Git resolution separates confirmed non-repositories from launch/I/O, timeout,
cancellation, malformed output, unsuccessful invocation, and unavailable paths.
Only confirmed non-repositories increment the repository skip count. A resolver
panic records failure and retains previous target diagnostics. Report/discovery
panics are task failures, not evidence of a disconnected transport.

The timer-driven coordinator and bounded queues are retained. There is no new
scheduler, actor framework, provider registry, or persistence database.
