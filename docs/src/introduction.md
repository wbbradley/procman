# Introduction

**procman** is a Foreman-like process supervisor written in Rust. It reads a
`.pman` configuration file, spawns all listed commands, and multiplexes
their output to the terminal with right-aligned name prefixes. When any child
exits or a signal arrives, procman tears everything down cleanly.

## Key Features

- **Dependency-aware startup ordering** — jobs can wait on HTTP health
  checks, TCP ports, file existence, file content, or the exit of another
  job before starting.
- **Multiplexed output** — every line is prefixed with the originating job
  name, right-aligned for easy scanning.
- **Per-process log files** — each job gets its own log in `procman-logs/`,
  plus a combined `procman.log`.
- **Process output** — a `once = true` job can write `KEY=VALUE` pairs to
  `$PROCMAN_OUTPUT`; downstream jobs reference them with `@job.KEY` syntax.
- **Fan-out** — use `for` blocks with globs, literal arrays, or ranges to
  spawn multiple instances of a job.
- **User-defined CLI arguments** — define typed arguments in a `config { }`
  block and pass them after `--` on the command line. Arg values are
  available as `args.name` expressions and flow into shell via env vars.
- **Conditional job execution** — use `job name if expr { }` to evaluate an
  expression before spawning; falsy results skip the job entirely.
- **Clean shutdown** — Ctrl-C sends SIGTERM to every child, waits 2 seconds,
  then sends SIGKILL to anything still running.

## Design Principles

The `.pman` DSL is built on three core ideas:

- **Declarative** — the DSL describes what to run and when, not how. Runtime
  semantics (polling, fan-out tracking, shutdown cascades) remain procman's
  domain.
- **Two worlds, clearly separated** — procman expressions use their own
  syntax. Shell blocks are opaque strings. Values flow into shell exclusively
  via environment variables. Procman never interpolates inside shell strings.
- **Strict typing** — type errors in expressions cause immediate shutdown.
  No silent coercion.

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
