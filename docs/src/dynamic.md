# Dynamic Process Management

Procman can accept new processes at runtime through a `serve` + `start`/`stop` pattern. This is
useful for scripted service bringup where some processes need to be added after health checks
pass or after initial setup completes.

## Overview

1. Start procman in serve mode: `procman serve &`
2. Wait for your services to become healthy.
3. Add workers dynamically: `procman start "redis-server --port 6380"`
4. When done, shut down cleanly: `procman stop`

## FIFO path

The FIFO is auto-derived from the config file path. The path is deterministic — given the same
config file, the FIFO path is always the same:

```
/tmp/procman-{parent_dir_name}-{path_hash}.fifo
```

Where `{parent_dir_name}` is the sanitized name of the config file's parent directory (up to 32
alphanumeric characters) and `{path_hash}` is a hex hash of the canonical config path. This
means different projects using different config files get separate FIFOs automatically.

## JSON wire protocol

Messages are sent as newline-delimited JSON. Each line is a `FifoMessage` with a `type` field.

### `run` message

Tells the server to spawn a new process.

```json
{
  "type": "run",
  "name": "redis-server",
  "run": "redis-server --port 6380",
  "env": {"REDIS_LOG": "verbose"},
  "depends": [{"url": "http://localhost:8080/health", "code": 200}],
  "once": true
}
```

| Field     | Required | Description |
|-----------|----------|-------------|
| `name`    | yes      | Process name (used in logs and deduplication). |
| `run`     | yes      | Command to execute. |
| `env`     | no       | Extra environment variables (merged with the server's env). |
| `depends` | no       | Dependencies, same format as in the [config file](dependencies.md). |
| `once`    | no       | If `true`, process is a one-shot task (default `false`). |

### `shutdown` message

Tells the server to begin graceful shutdown.

```json
{
  "type": "shutdown",
  "user": "wbbradley",
  "message": "User-initiated via CLI"
}
```

| Field     | Required | Description |
|-----------|----------|-------------|
| `user`    | no       | Who requested the shutdown (for logging). |
| `message` | no       | Reason for the shutdown (for logging). |

## Name deduplication

If the same `name` is sent more than once, subsequent instances are automatically renamed with a
numeric suffix: the second becomes `name.1`, the third `name.2`, and so on. The first use of a
name is never suffixed.

## Workflow example

```sh
# Start the supervisor in the background
procman serve &

# Wait for the API to become healthy
while ! curl -sf http://localhost:8080/health; do sleep 1; done

# Add a worker dynamically
procman start "redis-server --port 6380"

# Later, shut down everything
procman stop
```

## Error behavior

`procman start` and `procman stop` open the FIFO with `O_NONBLOCK`. If no `procman serve`
instance is listening, the open fails immediately with a clear error message — there is no
blocking or timeout.
