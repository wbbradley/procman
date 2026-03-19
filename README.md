# procman

[![crates.io](https://img.shields.io/crates/v/procman.svg)](https://crates.io/crates/procman)

A foreman-like process supervisor written in Rust. Reads a `procman.yaml`, spawns all listed commands, multiplexes their output with name prefixes, and tears everything down cleanly when any child exits or a signal arrives.

## Usage

```
cargo install --path .
```

### `procman run` — run all commands

```bash
procman run                  # uses ./procman.yaml
procman run myapp.yaml       # custom config path
```

Bare `procman` with no subcommand is equivalent to `procman run`.

### `procman serve` — accept dynamic commands via a FIFO

```bash
procman serve &
procman serve myapp.yaml &    # custom config path
```

Runs all procman.yaml commands and listens on a named FIFO for dynamically added commands. The FIFO path is derived automatically from the config file path, so you never need to specify it. The FIFO is created automatically and removed on exit.

### `procman start` — send a command to a running server

```bash
procman start "redis-server --port 6380"
procman start --config myapp.yaml "redis-server --port 6380"
```

Opens the FIFO for writing and sends a JSON message. Fails immediately if no server is listening. The FIFO path is derived from the config path, matching the running server.

### `procman stop` — gracefully shut down a running server

```bash
procman stop
procman stop myapp.yaml
```

Sends a shutdown command to the server via the FIFO. The server logs the request and terminates cleanly.

### Scripted service bringup

The `serve`/`start` pattern enables imperative orchestration — start a supervisor, wait for dependencies to become healthy, then add dependent services:

```bash
procman serve &
while ! curl -sf http://localhost:8080/health; do sleep 1; done
procman start "redis-server --port 6380"
```

An advisory `flock` on procman.yaml prevents multiple instances from managing the same file simultaneously.

## procman.yaml Format

```yaml
web:
  env:
    PORT: "3000"
  run: serve --port $PORT

migrate:
  run: db-migrate up
  once: true

api:
  depends:
    - process_exited: migrate
    - url: http://localhost:3000/health
      code: 200
      poll_interval: 0.5
      timeout_seconds: 30
  run: api-server start

setup:
  depends:
    - path: /tmp/ready.flag
  run: post-setup-task

db:
  depends:
    - tcp: "127.0.0.1:5432"
  run: db-client start

nodes:
  for_each:
    glob: "/etc/nodes/*.yaml"
    as: NODE_CONFIG
  run: node-agent --config $NODE_CONFIG
  once: true
```

- Each top-level key is a process name.
- `run` (required): the command to execute (parsed with POSIX shell quoting). Supports `${{ process.KEY }}` templates to reference output values from `once` dependencies.
- `env` (optional): per-process environment variables (also supports `${{ }}` templates).
- `once` (optional): if `true`, the process exits cleanly on success (code 0) without triggering supervisor shutdown. Processes can write key-value pairs to `$PROCMAN_OUTPUT` for downstream template resolution.
- `for_each` (optional): fan-out a template process across glob matches. Requires `glob` (pattern) and `as` (variable name). Each match spawns an instance with the variable set in env and substituted in the run string.
- `depends` (optional): list of dependencies that must be satisfied before the process starts. Circular dependencies are detected at config parse time. Dependency paths support `$VAR` and `${VAR}` environment variable expansion (including per-process `env` overrides); use `$$` for a literal `$`.
  - **HTTP health check**: `url` + `code` (expected status), with optional `poll_interval` and `timeout_seconds`.
  - **TCP connect**: `tcp` (address:port), with optional `poll_interval` and `timeout_seconds`.
  - **File exists**: `path` to a file that must exist.
  - **File contains key**: `file_contains` with `path`, `format` (json/yaml), `key` (dot-separated path), and optional `env` (variable name to extract the value into). With optional `poll_interval` and `timeout_seconds`.
  - **Process exited**: `process_exited` names a `once: true` process that must complete successfully before this process starts.

## Behavior

- Each child is signaled individually at shutdown.
- stderr is merged into stdout per-process.
- Output is prefixed with the process name, right-aligned and padded.
- Per-process logs are written to `./procman-logs/<name>.log` (directory is recreated each run).
- A combined `./procman-logs/procman.log` contains the full interleaved formatted output (same as stdout).
- On SIGINT or SIGTERM, all children receive SIGTERM. After a 2-second grace period, remaining processes are sent SIGKILL.
- procman exits with the first child's exit code.

## License

MIT
