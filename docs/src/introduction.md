# Introduction

**procman** is a Foreman-like process supervisor written in Rust. It reads a
`procman.yaml` configuration file, spawns all listed commands, and multiplexes
their output to the terminal with right-aligned name prefixes. When any child
exits or a signal arrives, procman tears everything down cleanly.

## Key Features

- **Dependency-aware startup ordering** — processes can wait on HTTP health
  checks, TCP ports, file existence, file content, or the exit of another
  process before starting.
- **Multiplexed output** — every line is prefixed with the originating process
  name, right-aligned for easy scanning.
- **Per-process log files** — each process gets its own log in `procman-logs/`,
  plus a combined `procman.log`.
- **Process output templates** — a process can write `key=value` pairs to a
  well-known file; downstream processes reference them with
  `${{ process.key }}` syntax.
- **Fan-out** — use `for_each` with a glob pattern to spawn multiple instances
  of a process template.
- **Dynamic process management** — `procman serve` listens on a FIFO so you can
  add processes at runtime with `procman start`.
- **Clean shutdown** — Ctrl-C sends SIGTERM to every child, waits 2 seconds,
  then sends SIGKILL to anything still running.

## Installation

```sh
cargo install procman
```

Or clone and build from source:

```sh
git clone https://github.com/wbbradley/procman.git
cd procman
cargo install --path .
```

## Links

- [GitHub repository](https://github.com/wbbradley/procman)
- [crates.io](https://crates.io/crates/procman)
