# procman

[![crates.io](https://img.shields.io/crates/v/procman.svg)](https://crates.io/crates/procman)
[![docs](https://img.shields.io/badge/docs-mdbook-blue)](https://wbbradley.github.io/procman/)

A foreman-like process supervisor written in Rust. Reads a YAML config file, spawns all listed jobs, multiplexes their output with name prefixes, and tears everything down cleanly when any child exits or a signal arrives. See the [full documentation](https://wbbradley.github.io/procman/) for detailed guides on configuration, dependencies, templates, and more.

## Usage

```
cargo install --path .
```

```bash
procman myapp.yaml                             # run all jobs
procman myapp.yaml -e PORT=3000 -e RUST_LOG=debug  # inject env vars
procman myapp.yaml -- --rust-log debug --verbose   # pass config-defined args
procman myapp.yaml --debug                     # pause before shutdown on failure
```

The first positional argument is the path to the config file (required). Arguments after `--` are parsed according to `config.args` definitions (see below).

### Dependency graph

Most service ordering is handled declaratively in the config file. Jobs with no `depends` list start immediately; jobs with dependencies are held until every condition is met. This forms a DAG — circular dependencies are detected at parse time.

```yaml
jobs:
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

Here `migrate` and `web` start immediately. `api` waits for `migrate` to exit successfully and for `web` to pass its health check — no scripting required. Available dependency types include HTTP health checks, TCP connect, file exists, file contains, process exited, and their negations. See the [Config Format](#config-format) section below and the [Dependencies chapter](https://wbbradley.github.io/procman/dependencies.html) for the complete reference.

### `-e` / `--env` — inject environment variables

A repeatable `-e KEY=VALUE` flag for ad-hoc environment variable injection without modifying the config file. Precedence (lowest → highest): system env → CLI `-e` → YAML `env:` block.

```bash
procman myapp.yaml -e PORT=3000 -e RUST_LOG=debug
```

### `--debug` — pause before shutdown

When a child process fails, procman pauses before sending SIGTERM, prints which process triggered the shutdown and which processes are still running, and waits for ENTER or Ctrl+C to proceed. Requires an interactive terminal.

```bash
procman myapp.yaml --debug
```

## Config Format

```yaml
config:
  logs: ./my-logs    # optional: custom log directory (default: procman-logs)
  args:              # optional: user-defined CLI arguments (parsed after --)
    rust_log:
      short: r
      description: "RUST_LOG configuration"
      type: string
      default: "info"
      env: RUST_LOG
    enable_feature:
      type: bool
      default: false
      env: FEATURE_ENABLED

jobs:
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

  recovery:
    run: ./scripts/recover.sh
    autostart: false

  web-watched:
    run: web-server --port 8080
    watch:
      - name: health
        check:
          url: http://localhost:8080/health
          code: 200
        initial_delay: 5.0
        poll_interval: 10.0
        failure_threshold: 3
        on_fail: shutdown
      - name: disk
        check:
          path: /var/run/healthy
        on_fail:
          spawn: recovery
```

The config file has two top-level keys:

- `config` (optional): global settings.
  - `logs` (optional): custom log directory path (default: `procman-logs`).
  - `args` (optional): map of user-defined CLI arguments parsed from argv after `--`. Each key is the arg name (underscores in YAML become dashes on the CLI, e.g. `rust_log` → `--rust-log`). Fields:
    - `type` (optional, default `string`): `string` or `bool`. String args take a value (`--name VALUE`), bool args are flags (`--name` = true).
    - `short` (optional): single-character or short string for `-s` shorthand.
    - `description` (optional): help text shown with `-- --help`.
    - `default` (optional): fallback value. Args without a default are required.
    - `env` (optional): environment variable name to inject the arg value into (e.g. `env: RUST_LOG`).
  - Arg values are available as `${{ args.var_name }}` templates in `run` and `env` fields.
  - Env precedence (lowest → highest): system env → arg defaults → CLI `--` args → `-e` flags → YAML `env:` blocks.
- `jobs` (required): map of job name to job definition.

Each job definition supports:

- `run` (required): the command to execute. All commands are passed to `sh -euo pipefail -c`, so shell features (pipes, redirects, `&&`, variable expansion) work in both single-line and multi-line commands. The strict flags mean unset variable references and pipeline failures are treated as errors. Supports `${{ process.KEY }}` templates to reference output values from `once` dependencies, and `${{ args.name }}` to reference user-defined arg values.
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
- `autostart` (optional, default `true`): if `false`, the process is dormant — it won't start until explicitly spawned via a watch `on_fail: spawn` action.
- `watch` (optional): list of runtime health checks that monitor the process after it starts. Each watch polls a check (same types as dependencies) and takes an action when consecutive failures exceed the threshold.
  - `name` (optional): human-readable name; auto-generated if omitted.
  - `check` (required): same syntax as a dependency (HTTP, TCP, file exists, etc.).
  - `initial_delay` (optional, default 0): seconds to wait before the first check.
  - `poll_interval` (optional, default 5): seconds between checks.
  - `failure_threshold` (optional, default 3): consecutive failures before triggering the action.
  - `on_fail` (optional, default `shutdown`): action to take — `shutdown`, `debug`, `log`, or `spawn: <process_name>` (starts a dormant process with `PROCMAN_WATCH_*` env vars).

## Behavior

- Each child runs in its own process group; shutdown signals reach all descendants.
- stderr is merged into stdout per-process.
- Output is prefixed with the process name, right-aligned and padded.
- Per-process logs are written to `<log_dir>/<name>.log` (directory is recreated each run; default `./procman-logs/`).
- A combined `<log_dir>/procman.log` contains the full interleaved formatted output (same as stdout).
- On SIGINT or SIGTERM, all children receive SIGTERM. After a 2-second grace period, remaining processes are sent SIGKILL.
- procman exits with the first child's exit code.

## License

MIT
