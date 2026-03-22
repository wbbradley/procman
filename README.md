# procman

[![crates.io](https://img.shields.io/crates/v/procman.svg)](https://crates.io/crates/procman)
[![docs](https://img.shields.io/badge/docs-mdbook-blue)](https://wbbradley.github.io/procman/)

A foreman-like process supervisor written in Rust. Reads a `procman.yaml`, spawns all listed commands, multiplexes their output with name prefixes, and tears everything down cleanly when any child exits or a signal arrives. See the [full documentation](https://wbbradley.github.io/procman/) for detailed guides on configuration, dependencies, templates, and more.

## Usage

```
cargo install --path .
```

### `procman run` — run all commands

```bash
procman run                  # uses ./procman.yaml
procman run myapp.yaml       # custom config path
procman run -e PORT=3000 -e RUST_LOG=debug  # inject env vars
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

### Dependency graph

Most service ordering is handled declaratively in `procman.yaml`. Processes with no `depends` list start immediately; processes with dependencies are held until every condition is met. This forms a DAG — circular dependencies are detected at parse time.

```yaml
migrate:
  run: db-migrate up
  once: true

web:
  run: serve --port 3000

api:
  depends:
    - process_exited: migrate
    - url: http://localhost:3000/health
      code: 200
      timeout_seconds: 30
  run: api-server start
```

Here `migrate` and `web` start immediately. `api` waits for `migrate` to exit successfully and for `web` to pass its health check — no scripting required. Available dependency types include HTTP health checks, TCP connect, file exists, file contains, process exited, and their negations. See the [procman.yaml Format](#procmanyaml-format) section below and the [Dependencies chapter](https://wbbradley.github.io/procman/dependencies.html) for the complete reference.

### Scripted service bringup (escape hatch)

When the declarative dependency graph isn't sufficient — for example, when you need to interact with an external system that procman can't poll — the `serve`/`start` pattern provides an imperative fallback:

```bash
procman serve &
while ! curl -sf http://localhost:8080/health; do sleep 1; done
procman start "redis-server --port 6380"
```

Prefer `depends` in `procman.yaml` over this pattern when possible. An advisory `flock` on procman.yaml prevents multiple instances from managing the same file simultaneously.

### `-e` / `--env` — inject environment variables

The `run`, `serve`, and `start` subcommands accept a repeatable `-e KEY=VALUE` flag for ad-hoc environment variable injection without modifying `procman.yaml`. Precedence (lowest → highest): system env → CLI `-e` → YAML `env:` block.

```bash
procman run -e PORT=3000 -e RUST_LOG=debug
procman start "my-worker" -e DB_URL=postgres://localhost/mydb
```

### `--debug` — pause before shutdown

The `run` and `serve` subcommands accept a `--debug` flag. When a child process fails, procman pauses before sending SIGTERM, prints which process triggered the shutdown and which processes are still running, and waits for ENTER or Ctrl+C to proceed. Requires an interactive terminal.

```bash
procman run --debug
procman serve --debug
```

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

healthcheck:
  depends:
    - not_listening: "127.0.0.1:8080"
    - not_exists: /tmp/api.lock
    - not_running: "old-api.*"
  run: api-server --port 8080

nodes:
  for_each:
    glob: "/etc/nodes/*.yaml"
    as: NODE_CONFIG
  run: node-agent --config $NODE_CONFIG
  once: true
```

- Each top-level key is a process name.
- `run` (required): the command to execute. All commands are passed to `sh -c`, so shell features (pipes, redirects, `&&`, variable expansion) work in both single-line and multi-line commands. Supports `${{ process.KEY }}` templates to reference output values from `once` dependencies.
- `env` (optional): per-process environment variables (also supports `${{ }}` templates).
- `once` (optional): if `true`, the process exits cleanly on success (code 0) without triggering supervisor shutdown. Processes can write key-value pairs to `$PROCMAN_OUTPUT` for downstream template resolution.
- `for_each` (optional): fan-out a template process across glob matches. Requires `glob` (pattern) and `as` (variable name). Each match spawns an instance with the variable set in env and substituted in the run string.
- `depends` (optional): list of dependencies that must be satisfied before the process starts. Circular dependencies are detected at config parse time. Dependency fields (`url`, `tcp`, `path`, `file_contains.path`, `not_listening`, `not_exists`, `not_running`) support `$VAR` and `${VAR}` environment variable expansion (including per-process `env` overrides); use `$$` for a literal `$`.
  - **HTTP health check**: `url` + `code` (expected status), with optional `poll_interval` and `timeout_seconds`.
  - **TCP connect**: `tcp` (address:port), with optional `poll_interval` and `timeout_seconds`.
  - **File exists**: `path` to a file that must exist.
  - **File contains key**: `file_contains` with `path`, `format` (json/yaml), `key` (JSONPath expression per RFC 9535, e.g. `$.database.url`), and optional `env` (variable name to extract the value into). With optional `poll_interval` and `timeout_seconds`.
  - **Process exited**: `process_exited` names a `once: true` process that must complete successfully before this process starts.
  - **TCP not listening**: `not_listening` (address:port), with optional `poll_interval` and `timeout_seconds`. Waits until no service is accepting connections.
  - **File not exists**: `not_exists` path that must not exist.
  - **Process not running**: `not_running` pattern (matched via `pgrep -f`). Waits until no matching process is found.

  All dependency types accept an optional `retry` (default `true`). Set `retry: false` to fail immediately if the dependency is not satisfied on the first check — useful to catch stale state (leftover lock files, ports still bound, zombie processes).

## Behavior

- Each child runs in its own process group; shutdown signals reach all descendants.
- stderr is merged into stdout per-process.
- Output is prefixed with the process name, right-aligned and padded.
- Per-process logs are written to `./procman-logs/<name>.log` (directory is recreated each run).
- A combined `./procman-logs/procman.log` contains the full interleaved formatted output (same as stdout).
- On SIGINT or SIGTERM, all children receive SIGTERM. After a 2-second grace period, remaining processes are sent SIGKILL.
- procman exits with the first child's exit code.

## License

MIT
