# Compatibility and qualification

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
binary. Qualification results and platform limits are recorded below as checks run.
