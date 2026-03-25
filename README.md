# procman

[![crates.io](https://img.shields.io/crates/v/procman.svg)](https://crates.io/crates/procman)
[![docs](https://img.shields.io/badge/docs-mdbook-blue)](https://wbbradley.github.io/procman/)

A foreman-like process supervisor written in Rust. Reads a `.pman` config file, spawns all listed jobs, multiplexes their output with name prefixes, and tears everything down cleanly when any child exits or a signal arrives. See the [full documentation](https://wbbradley.github.io/procman/) for detailed guides on configuration, dependencies, templates, and more.

## Usage

```
cargo install --path .
```

```bash
procman myapp.pman                             # run all jobs
procman myapp.pman -e PORT=3000 -e RUST_LOG=debug  # inject env vars
procman myapp.pman -- --rust-log debug --verbose   # pass config-defined args
procman myapp.pman --debug                     # pause before shutdown on failure
```

The first positional argument is the path to the config file (required). Arguments after `--` are parsed according to `config { arg ... { } }` definitions (see below).

### Dependency graph

Most service ordering is handled declaratively in the config file. Jobs with no `wait` block start immediately; jobs with wait conditions are held until every condition is met. This forms a DAG — circular dependencies are detected at parse time.

```
job migrate {
  once = true
  run "db-migrate up"
}

job web {
  run "serve --port 3000"
}

job api {
  wait {
    after @migrate
    http "http://localhost:3000/health" {
      status = 200
      timeout = 30s
    }
  }
  run "api-server start"
}
```

Here `migrate` and `web` start immediately. `api` waits for `migrate` to exit successfully and for `web` to pass its health check — no scripting required. Available wait condition types include HTTP health checks, TCP connect, file exists, file contains, process exited (`after`), and their negations. See the [Config Format](#config-format) section below and the [Dependencies chapter](https://wbbradley.github.io/procman/dependencies.html) for the complete reference.

### `-e` / `--env` — inject environment variables

A repeatable `-e KEY=VALUE` flag for ad-hoc environment variable injection without modifying the config file. Precedence (lowest → highest): system env → CLI `-e` → global `config { env { } }` → per-job `env` → per-iteration `for` bindings.

```bash
procman myapp.pman -e PORT=3000 -e RUST_LOG=debug
```

### `--debug` — pause before shutdown

When a child process fails, procman pauses before sending SIGTERM, prints which process triggered the shutdown and which processes are still running, and waits for ENTER or Ctrl+C to proceed. Requires an interactive terminal.

```bash
procman myapp.pman --debug
```

## Config Format

~~~
config {
  logs = "./my-logs"

  env {
    RUST_LOG = args.log_level
  }

  arg port {
    type = string
    default = "3000"
    short = "p"
    description = "Port to listen on"
  }

  arg log_level {
    type = string
    default = "info"
    short = "r"
    description = "RUST_LOG configuration"
  }

  arg enable_worker {
    type = bool
    default = false
  }
}

job migrate {
  once = true
  run ```
    ./run-migrations
    echo "DATABASE_URL=postgres://localhost:5432/mydb" > $PROCMAN_OUTPUT
  ```
}

job web {
  env PORT = args.port
  run "serve --port $PORT"
}

job api {
  env DB_URL = @migrate.DATABASE_URL

  wait {
    after @migrate
    http "http://localhost:3000/health" {
      status = 200
      timeout = 30s
      poll = 500ms
    }
  }

  run "api-server start --db $DB_URL"
}

job db {
  wait {
    connect "127.0.0.1:5432"
  }
  run "db-client start"
}

job healthcheck {
  wait {
    !connect "127.0.0.1:8080"
    !exists "/tmp/api.lock"
    !running "old-api.*"
  }
  run "api-server --port 8080"
}

job worker if args.enable_worker {
  run "worker-service start"
}

job nodes {
  once = true
  for config_path in glob("/etc/nodes/*.yaml") {
    env NODE_CONFIG = config_path
    run "node-agent --config $NODE_CONFIG"
  }
}

job web-watched {
  run "web-server --port 8080"

  watch health {
    http "http://localhost:8080/health" {
      status = 200
    }
    initial_delay = 5s
    poll = 10s
    threshold = 3
    on_fail shutdown
  }

  watch disk {
    exists "/var/run/healthy"
    on_fail spawn @recovery
  }
}

event recovery {
  run "./scripts/recover.sh"
}
~~~

The config file contains top-level blocks in any order:

- `config { }` (optional): global settings.
  - `logs` (optional): custom log directory path (default: `procman-logs`). Recreated each run.
  - `env { }` (optional): global environment variable bindings applied to all jobs. Overridable per-job.
  - `arg name { }` (optional): user-defined CLI arguments parsed from argv after `--`. Underscores in names become dashes on the CLI (e.g. `log_level` → `--log-level`). Fields:
    - `type` (optional, default `string`): `string` or `bool`. String args take a value (`--name VALUE`), bool args are flags (`--name` = true).
    - `short` (optional): single-character shorthand for `-s` form.
    - `description` (optional): help text shown with `-- --help`.
    - `default` (optional): fallback value. Args without a default are required.
  - Arg values are referenced in expressions as `args.name`. There is no `env` field on args — use `config { env { } }` to explicitly bind args to environment variables.
  - Env precedence (lowest → highest): system env → CLI `-e` → global `config { env { } }` → per-job `env` → per-iteration `for` bindings.
- `job name { }` / `job name if expr { }` — process definitions.
- `event name { }` — dormant processes, only started via `on_fail spawn @name`.

Each job definition supports:

- `run` (required): the command to execute. Inline `"..."` or fenced triple-backtick block. All commands are passed to `sh -euo pipefail -c`, so shell features (pipes, redirects, `&&`, variable expansion) work. The strict flags mean unset variable references and pipeline failures are treated as errors.
- `env` (optional): per-job environment variables. Single `env KEY = expr` or `env { }` block. Supports `args.name` references and `@job.KEY` output references.
- `once` (optional): if `true`, the process exits cleanly on success (code 0) without triggering supervisor shutdown. Processes can write key-value pairs to `$PROCMAN_OUTPUT` for downstream resolution via `@job.KEY`.
- `for VAR in iterable { }` (optional): fan-out a job across an iterable. Supported iterables: `glob("pattern")`, `["a", "b"]`, `0..3` (exclusive range), `0..=3` (inclusive range). Each iteration spawns an instance with the variable bound.
- `wait { }` (optional): block of conditions that must all be satisfied before `run` executes. Circular dependencies are detected at parse time. Condition types:
  - `after @job` — wait for a `once = true` job to exit successfully.
  - `http "url" { status = N }` — HTTP GET returns expected status, with optional `timeout` and `poll`.
  - `connect "host:port"` — TCP port accepts connections.
  - `!connect "host:port"` — TCP port stops accepting connections.
  - `exists "path"` — file exists on disk.
  - `!exists "path"` — file does not exist.
  - `!running "pattern"` — no process matches pattern (`pgrep -f`).
  - `contains "path" { format, key, var }` — file contains a key; optionally binds to a local `var`.
  - All conditions accept optional `timeout` (default `60s`), `poll` (default `1s`), and `retry` (default `true`; `false` = fail immediately on first check).
- `if expr` (optional, on the `job` line): expression evaluated before spawning. If falsy, the job is skipped entirely. Skipped `once = true` jobs register as exited so `after @job` dependents can proceed.
- `watch name { }` (optional): named runtime health checks that monitor the job after it starts. Each watch polls a condition (same types as `wait`) and takes an action when consecutive failures exceed the threshold.
  - `initial_delay` (optional, default `0s`): time before the first check.
  - `poll` (optional, default `5s`): time between checks.
  - `threshold` (optional, default `3`): consecutive failures before triggering the action.
  - `on_fail` (optional, default `shutdown`): action — `shutdown`, `debug`, `log`, or `spawn @event_name`.

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
