# Configuration Reference

Procman reads a single YAML file (passed as a positional argument) with two top-level keys:
`config` (optional) and `jobs` (required).

## Top-level structure

```yaml
config:
  logs: ./my-logs
  args:
    rust_log:
      type: string
      default: "info"
      env: RUST_LOG

jobs:
  web:
    run: serve --port 8080
  worker:
    run: cargo run --bin worker
```

- **`config`** (optional): global settings — custom log directory and user-defined CLI arguments.
- **`jobs`** (required): map of job name to job definition. Job names become the labels used in
  log output, dependency references, and template expressions.

## `config.logs`

Optional custom log directory path. Defaults to `procman-logs`. The directory is recreated on
each run.

```yaml
config:
  logs: ./my-logs
```

## `config.args`

Define CLI arguments that users can pass after `--` on the command line. Each key is the
argument name — underscores in YAML become dashes on the CLI (e.g. `rust_log` becomes
`--rust-log`).

```yaml
config:
  args:
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
```

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `type` | string | no | `string` | `string` or `bool`. String args take a value (`--name VALUE`), bool args are flags (`--name` = true). |
| `short` | string | no | — | Single character for `-s` shorthand. |
| `description` | string | no | — | Help text shown with `-- --help`. |
| `default` | any | no | — | Fallback value. Args without a default are **required**. |
| `env` | string | no | — | Environment variable name to inject the arg value into. |

Arg values are available as `${{ args.var_name }}` templates in `run`, `env`, and `condition`
fields:

```yaml
config:
  args:
    port:
      type: string
      default: "3000"

jobs:
  web:
    run: serve --port ${{ args.port }}
```

Running `procman myapp.yaml -- --help` prints generated usage based on the `config.args`
definitions.

**Environment variable precedence** (lowest to highest):

| Source | Priority |
|--------|----------|
| System environment | lowest |
| Arg defaults | |
| CLI `--` args | |
| `-e` flags | |
| YAML `env:` blocks | highest |

## Fields

### `run` (required)

The command to execute. All commands are passed to `sh -euo pipefail -c`, so shell features like
pipes, redirects, `&&`, variable expansion, and multi-line scripts all work naturally. The strict
flags mean unset variable references and mid-pipeline failures are treated as errors:

```yaml
jobs:
  api:
    run: cargo run --release --bin api-server

  migrate:
    run: |
      ./run-migrations
      echo "DATABASE_URL=postgres://localhost:5432/mydb" > $PROCMAN_OUTPUT

  healthcheck:
    run: curl -s http://localhost:8080/health && echo "OK"
```

The `run` field also supports [template references](templates.md) (`${{ process.key }}`) and
arg templates (`${{ args.name }}`).

An empty or whitespace-only `run` value is rejected at parse time.

### `env` (optional)

A map of extra environment variables merged into the process's environment. The OS environment
is inherited first, then these values are layered on top (overriding any collisions).

```yaml
jobs:
  worker:
    env:
      RUST_LOG: debug
      PORT: "3000"
    run: my-server --port 3000
```

Values may contain [template references](templates.md):

```yaml
jobs:
  app:
    env:
      DB_URL: "${{ migrate.DATABASE_URL }}"
    run: ./start-app
    depends:
      - process_exited: migrate
```

### `once` (optional, default `false`)

When `true`, the process is expected to run to completion. An exit code of 0 is treated as
success and does **not** trigger supervisor shutdown. A non-zero exit code still triggers
shutdown.

Processes with `once: true` can write key-value output to their `$PROCMAN_OUTPUT` file, which
other processes can read via [template references](templates.md).

```yaml
jobs:
  migrate:
    run: ./run-migrations
    once: true
```

### `depends` (optional)

A list of [dependency](dependencies.md) objects that must all be satisfied before the process
is started. See the [Dependencies](dependencies.md) chapter for the full reference.

```yaml
jobs:
  api:
    depends:
      - url: http://localhost:8080/health
        code: 200
      - process_exited: migrate
    run: ./start-api
```

### `condition` (optional)

A shell command evaluated before spawning the process. If it exits non-zero, the job is
skipped entirely. The command runs via `sh -euo pipefail -c` in the process's resolved
environment.

```yaml
jobs:
  optional-worker:
    condition: test -n "$WORKER_ENABLED"
    run: worker-service start
```

Key behaviors:

- Skipped `once: true` jobs are registered as exited, so `process_exited` dependents can
  proceed normally.
- The condition runs in the process's resolved environment (including `env:` overrides and
  injected arg values).
- Supports `${{ args.name }}` templates:

```yaml
config:
  args:
    enable_worker:
      type: bool
      default: false
      env: WORKER_ENABLED

jobs:
  worker:
    condition: test "$WORKER_ENABLED" = "true"
    run: worker-service start
```

### `for_each` (optional)

Fan-out configuration that spawns one instance of the process per glob match. Requires two
sub-fields:

| Field | Type | Description |
|-------|------|-------------|
| `glob` | string | Glob pattern to match files |
| `as` | string | Environment variable name that receives the matched path |

Each glob match spawns a separate process instance. The variable named by `as` is set in the
instance's environment and substituted into the `run` string.

```yaml
jobs:
  nodes:
    for_each:
      glob: "configs/node-*.yaml"
      as: CONFIG_PATH
    run: ./start-node --config $CONFIG_PATH
    once: true
```

Fan-out group completion is tracked so that `process_exited` dependencies on the template
process name work transparently — the dependency is satisfied only once **all** instances
have exited.

### `autostart` (optional, default `true`)

When `false`, the process is dormant — it won't start until explicitly spawned via a watch
`on_fail: spawn` action.

```yaml
jobs:
  recovery:
    run: ./scripts/recover.sh
    autostart: false
```

### `watch` (optional)

A list of runtime health checks that monitor the process after it starts. Each watch polls
a check (same types as dependencies) and takes an action when consecutive failures exceed the
threshold.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `name` | string | auto | Human-readable name |
| `check` | object | — | Same syntax as a dependency (HTTP, TCP, file exists, etc.) |
| `initial_delay` | float | 0 | Seconds to wait before the first check |
| `poll_interval` | float | 5 | Seconds between checks |
| `failure_threshold` | integer | 3 | Consecutive failures before triggering the action |
| `on_fail` | string/object | `shutdown` | Action: `shutdown`, `debug`, `log`, or `spawn: <process_name>` |

```yaml
jobs:
  web:
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

## Environment variable expansion

Dependency fields (`url`, `tcp`, `path`, `file_contains.path`, `not_listening`,
`not_exists`, `not_running`) support environment variable expansion at parse time:

| Syntax | Behavior |
|--------|----------|
| `$VAR` | Replaced with the value of `VAR` |
| `${VAR}` | Replaced with the value of `VAR` (braced form) |
| `$$` | Escaped literal `$` |

If a referenced variable is not set, procman exits with an error identifying the
undefined variable.

```yaml
jobs:
  api:
    depends:
      - path: $HOME/.config/ready.flag
      - url: http://localhost:${API_PORT}/health
        code: 200
    run: ./start-api
```

## Parse-time validation

Procman validates the configuration at parse time and exits with an error if any of these
checks fail:

- **Non-empty `run`**: every process must have a non-empty run command. Shell syntax errors
  are reported at runtime by `sh`.
- **Dependency graph cycles**: `process_exited` dependencies are checked for circular
  references using a DFS traversal. The error message shows the full cycle path
  (e.g. `circular dependency: a -> b -> c -> a`).
- **Unknown dependencies**: a `process_exited` dependency referencing a process name not
  defined in the config is rejected.
- **Template validation**: template references (`${{ process.key }}`) are checked against three
  rules — see [Templates](templates.md) for details.
