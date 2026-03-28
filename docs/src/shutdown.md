# Shutdown & Signals

When procman shuts down, it ensures every child process — and all of its descendants — receives
a termination signal and is cleaned up.

## What triggers shutdown

Shutdown begins when either:

- A child process exits (unless it is a `job` and exits with code 0).
- The user sends Ctrl-C (SIGINT) or SIGTERM to procman.

## Process groups

Each child process runs in its own **process group** (`setpgid(0, 0)` is called before exec).
This means:

- Signals sent to the group reach every descendant of the child, not just the top-level PID.
- Unrelated processes managed by procman do not receive each other's signals.

This is especially important for **multi-line `run` commands**, which are executed via `sh -euo pipefail -c`.
The shell spawns child processes (pipes, subshells, backgrounded commands) that would otherwise
be orphaned on shutdown. Because the entire process group is signaled, those descendants are
cleaned up together with the shell.

## Shutdown sequence

1. **SIGTERM** is sent to each remaining child's process group (`killpg`).
2. Procman waits up to **2 seconds** for processes to exit cleanly.
3. Any process groups still alive after the grace period receive **SIGKILL**.
4. Procman waits for all remaining processes to be reaped.

## Exit code

Procman's exit code is the exit code of the **first process that terminated** (the one that
triggered shutdown). If the first process was killed by a signal rather than exiting normally,
the exit code is `1`.
