# CLI Reference

## `procman <CONFIG> [OPTIONS] [-- ARGS]`

Spawn all processes defined in the config file and wait for exit or signal.

- **CONFIG** is a required positional argument — the path to the `.pman` config file.
- Acquires an exclusive advisory lock on the config file to prevent concurrent instances.
- On SIGINT or SIGTERM, initiates [graceful shutdown](shutdown.md).

```sh
procman myapp.pman
procman myapp.pman -e PORT=3000 -e RUST_LOG=debug
procman myapp.pman --debug
procman myapp.pman --check                     # validate config and exit
procman myapp.pman -- --rust-log debug --verbose
```

## `-e` / `--env` — Extra environment variables

A repeatable `-e KEY=VALUE` flag to inject environment variables without modifying the
config file.

```sh
procman myapp.pman -e PORT=3000 -e RUST_LOG=debug
```

## `-- [ARGS]` — User-defined arguments

Arguments after `--` are parsed according to the `config.args` definitions in the config
file. See the [Configuration](configuration.md#configargs) chapter for how to define args.

```sh
procman myapp.pman -- --rust-log debug --enable-feature
```

Running `-- --help` prints generated usage based on the `config.args` definitions:

```sh
procman myapp.pman -- --help
```

This shows each defined argument's name, type, description, default value, and short form.

## `--check` — Validate config and exit

The `--check` flag runs the full config parse and validation pipeline — arg definitions,
template resolution, dependency graph cycle detection, output reference validation, watch
uniqueness, and all other static checks — then prints a success message and exits without
starting any processes.

```sh
procman myapp.pman --check
```

On success, prints `<path>: ok` and exits with code 0. On failure, prints the error and exits
non-zero. This is useful for:

- **Editor integration** — run `--check` on save for instant feedback.
- **CI pipelines** — catch config errors before deployment.
- **Quick validation** — verify a config without spawning anything.

No signal handlers, loggers, or processes are created.

## `--debug` — Pause before shutdown

The `--debug` flag pauses the shutdown sequence when a child process fails or a dependency
times out, giving you time to inspect remaining processes before they are terminated.

```sh
procman myapp.pman --debug
```

When triggered, procman prints:
- Which process caused the shutdown (name, PID, exit code or signal)
- A list of processes still running (name and PID)
- A prompt to press ENTER (or Ctrl+C) to continue with the normal shutdown sequence

The `--debug` flag requires an interactive terminal (stdin must be a TTY). If stdin is not
a TTY, procman exits immediately with an error.

## Environment variable precedence

**Precedence (lowest → highest):**

| Source | Priority |
|--------|----------|
| System environment | lowest |
| CLI `-e` flags | |
| Global `config { env { } }` | |
| Per-job `env` | |
| Per-iteration `for` bindings | highest |

Per-iteration `for` bindings win over per-job `env`, which wins over global `config { env { } }`,
which wins over CLI `-e` flags, which win over inherited system environment variables.

## File locking

Procman acquires an **exclusive advisory lock** (`flock`) on the config file before starting.
If another procman instance is already running with the same config, the second instance exits
immediately with an error message.

## Exit code

Procman's exit code is the exit code of the **first process that terminated** (the one that
triggered shutdown). If the first termination was caused by a signal rather than a normal exit,
the exit code is `1`.

## Signals

On SIGINT (Ctrl-C) or SIGTERM, procman initiates [graceful shutdown](shutdown.md): SIGTERM is
sent to each child's process group, followed by a 2-second grace period, then SIGKILL for any
stragglers.
