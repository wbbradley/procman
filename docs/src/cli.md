# CLI Reference

Procman provides four subcommands. If no subcommand is given, `run` is used by default.

## `procman run [CONFIG]`

Spawn all processes defined in the config file and wait for exit or signal.

- **CONFIG** defaults to `procman.yaml`.
- Acquires an exclusive advisory lock on the config file to prevent concurrent instances.
- On SIGINT or SIGTERM, initiates [graceful shutdown](shutdown.md).

```sh
procman run                 # uses procman.yaml
procman run services.yaml   # uses a custom config
```

## `procman serve [CONFIG]`

Like `run`, but also listens on a FIFO for dynamically added processes. See the
[Dynamic Process Management](dynamic.md) chapter for details.

- **CONFIG** defaults to `procman.yaml`.
- The FIFO path is auto-derived from the config path (deterministic, based on the parent
  directory name and a path hash).
- Also acquires an exclusive advisory lock on the config file.

```sh
procman serve &
```

## `procman start COMMAND [--config CONFIG]`

Send a run command to a running `procman serve` instance.

- **COMMAND** is the full command line to run. The process name is derived from the program
  basename (e.g. `"redis-server --port 6380"` runs as `redis-server`).
- **--config** defaults to `procman.yaml` and is used to derive the FIFO path.
- Fails immediately with a clear error if no server is listening (uses `O_NONBLOCK`).

```sh
procman start "redis-server --port 6380"
procman start "worker --threads 4" --config services.yaml
```

## `procman stop [CONFIG]`

Send a shutdown command to a running `procman serve` instance.

- **CONFIG** defaults to `procman.yaml`.
- Fails immediately if no server is listening.

```sh
procman stop
```

## File locking

Both `run` and `serve` acquire an **exclusive advisory lock** (`flock`) on the config file
before starting. If another procman instance is already running with the same config, the
second instance exits immediately with an error message.

## Exit code

Procman's exit code is the exit code of the **first process that terminated** (the one that
triggered shutdown). If the first termination was caused by a signal rather than a normal exit,
the exit code is `1`.

## Signals

On SIGINT (Ctrl-C) or SIGTERM, procman initiates [graceful shutdown](shutdown.md): SIGTERM is
sent to each child's process group, followed by a 2-second grace period, then SIGKILL for any
stragglers.
