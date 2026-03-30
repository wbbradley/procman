# The .pman Language — Design Spec

## Design Principles

- **Declarative** — `.pman` describes what to run and when, not how. Runtime semantics (polling, fan-out tracking, shutdown cascades) remain procman's domain.
- **Two worlds, clearly separated** — procman expressions use their own syntax. Shell blocks are opaque strings. Values flow into shell exclusively via environment variables. Procman never interpolates inside shell strings.
- **Strict typing** — type errors in expressions cause immediate shutdown. No silent coercion.
- **Fail early** — as much validation as possible at parse time.

## File Format

Extension: `.pman`

Comments: `#` to end of line.

### Identifiers

Job names, event names, arg names, and variable names are identifiers. Valid identifiers match `[a-zA-Z_][a-zA-Z0-9_-]*` — they start with a letter or underscore, followed by letters, digits, underscores, or hyphens.

### String Literals

String literals are double-quoted. Supported escape sequences: `\"` (literal quote), `\\` (literal backslash), `\n` (newline), `\t` (tab). No other backslash escapes are recognized.

### Duration Literals

Duration literals are a number followed by a unit suffix: `s` (seconds), `ms` (milliseconds), `m` (minutes). Fractional values are allowed (e.g., `1.5s`). No other units in v1.

### The `none` Literal

`none` represents the absence of a value. It is valid only in specific positions: `timeout = none` (infinite wait), `default = none` (no default). Using `none` in env value positions or boolean contexts is a parse-time error.

## Top-Level Blocks

A `.pman` file contains top-level blocks in any order:

- `config { }` — global settings (logs, log_time)
- `arg name { }` — CLI argument declaration
- `env { }` / `env KEY = expr` — global environment variable bindings
- `job name { }` — one-shot process (runs to completion)
- `job name if expr { }` — conditionally evaluated one-shot job
- `service name { }` — long-running daemon process
- `service name if expr { }` — conditionally evaluated service
- `event name { }` — dormant process, only started via `on_fail spawn`

## Config Block

```
config {
  logs = "./my-logs"
  log_time = true
}
```

### `config.logs`

Optional log directory path. Defaults to `logs/procman`. Recreated each run.

### `config.log_time`

Optional boolean. When `true`, every log line is prefixed with elapsed time since procman started (e.g., `api 1.2s | listening on :3000`). Defaults to `false`.

## Env Block

Global environment variable bindings applied to all jobs. Overridable per-job. Declared at the top level.

Block form:
```
env {
  RUST_LOG = args.log_level
  PORT = "3000"
}
```

Single-binding form:
```
env RUST_LOG = args.log_level
```

Both forms can appear multiple times and coexist in the same file.

## Arg Declarations

CLI arguments parsed after `--`. Declared at the top level, outside `config`.

```
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

arg enable_feature {
  type = bool
  default = false
}
```

Underscores become dashes on the CLI (`log_level` -> `--log-level`).

| Field | Required | Default | Description |
|-------|----------|---------|-------------|
| `type` | no | `string` | `string` or `bool` |
| `short` | no | — | Single character shorthand |
| `description` | no | — | Help text for `-- --help` |
| `default` | no | — | Fallback value. Args without a default are required. |

Arg values are referenced in expressions as `args.name`. There is no `env` field on args — use a top-level `env { }` block to explicitly bind args to environment variables.

### Env Precedence

Lowest to highest:

1. System env (inherited)
2. CLI `-e KEY=VALUE` flags
3. Top-level `env { }`
4. Per-job `env`
5. Per-iteration `for` bindings

Note: `var` bindings from `contains` conditions are procman expressions, not direct env injections. They enter the environment only when explicitly assigned via `env KEY = var_name`.

## Job and Service Definitions

A `job` is a one-shot process that runs to completion. Exit code 0 is treated as success without triggering supervisor shutdown. Jobs can write key-value output to `$PROCMAN_OUTPUT` for downstream references via `@job.KEY`.

A `service` is a long-running daemon process that runs for the lifetime of the supervisor. If a service exits, it triggers shutdown.

```
job migrate {
  run """
    ./run-migrations
    echo "DATABASE_URL=postgres://localhost:5432/mydb" > $PROCMAN_OUTPUT
  """
}

service api {
  env DB_URL = @migrate.DATABASE_URL
  env {
    API_KEY = "secret"
    LOG_DIR = args.log_dir
  }

  wait {
    after @migrate
    http "http://localhost:3000/health" {
      status = 200
      timeout = 30s
      poll = 500ms
    }
  }

  run "start-api --db $DB_URL"
}
```

### Fields

| Field | Required | Description |
|-------|----------|-------------|
| `run` | yes | Shell command — inline `"..."` or fenced triple-quote block |
| `env` | no | Single `env KEY = expr` or `env { }` block. Both styles can coexist. |
| `wait` | no | Block of conditions, all must pass before `run` |
| `if` | no | Expression on the `job`/`service` line: `job name if expr { }` |
| `watch` | no | Named runtime health check blocks (services only) |
| `for` | no | Iteration block wrapping `env`/`run` |

### Shell Blocks

Inline:
```
run "echo hello"
```

Multi-line fenced:
~~~
run """
  ./run-migrations
  echo "DATABASE_URL=postgres://localhost:5432/mydb" > $PROCMAN_OUTPUT
"""
~~~

Procman never interpolates inside shell strings. Values flow in exclusively via environment variables.

### Conditional Jobs and Services

```
service worker if args.enable_worker {
  run "worker-service start"
}
```

If the expression is falsy, the job/service is not evaluated at all — no dependency waiting, no env resolution. Skipped jobs still register as exited so `after @job` dependents can proceed.

## Fan-Out (`for`)

The `for` block lives inside a job or service and wraps `env` and `run`. It iterates over a typed iterable, binding a local variable per iteration:

```
job nodes {
  wait {
    after @setup
  }

  for config_path in glob("configs/node-*.yaml") {
    env NODE_CONFIG = config_path
    run "start-node --config $NODE_CONFIG"
  }
}
```

### Iterables

| Syntax | Description |
|--------|-------------|
| `glob("pattern")` | File glob, evaluated at runtime (after `wait` conditions are satisfied), sorted lexicographically. Zero matches is a runtime error. |
| `["a", "b", "c"]` | Literal array of strings |
| `0..3` | Exclusive range: 0, 1, 2 |
| `0..=3` | Inclusive range: 0, 1, 2, 3 |

### Scoping

- The iteration variable is scoped to the `for` block
- It shares the local variable namespace with `var` bindings from `contains` conditions
- `args.x` and `@job.KEY` have distinct syntactic prefixes and cannot collide with bare local names
- Shadowing any existing local variable name is a parse-time error
- Lowercase is convention, not enforced

### Instance Naming

`{job_name}-{index}` (0-based). Three glob matches on `nodes` produce `nodes-0`, `nodes-1`, `nodes-2`.

### Group Completion

`after @nodes` in another job's `wait` block is satisfied only when all instances have exited successfully.

### Env Inheritance

`env` bindings outside the `for` block apply to all instances. Bindings inside are per-iteration:

```
job nodes {
  env CLUSTER = "prod"

  for config_path in glob("configs/*.yaml") {
    env NODE_CONFIG = config_path
    run "start-node --config $NODE_CONFIG --cluster $CLUSTER"
  }
}
```

## Wait Conditions

The `wait` block contains conditions evaluated sequentially. Each must be satisfied before the next is checked. All must pass before `run` executes.

```
wait {
  after @migrate
  connect "127.0.0.1:5432"
  http "http://localhost:8080/health" {
    status = 200
    timeout = 30s
    poll = 500ms
  }
  exists "/tmp/ready.flag"
  contains "/tmp/config.yaml" {
    format = "yaml"
    key = "$.database.url"
    var = database_url
  }
  !connect "127.0.0.1:8080"
  !exists "/tmp/api.lock"
  !running "old-api.*"
}
```

### Condition Types

| Syntax | Description |
|--------|-------------|
| `after @job` | Wait for a job to exit successfully. Parse-time error if the target is not a `job`. |
| `http "url" { status = N }` | HTTP GET returns expected status |
| `connect "host:port"` | TCP port accepts connections |
| `!connect "host:port"` | TCP port stops accepting connections |
| `exists "path"` | File exists on disk |
| `!exists "path"` | File does not exist |
| `!running "pattern"` | No process matches pattern (`pgrep -f`). No positive `running` form — "wait until a process is running" is inherently racy; use `connect` or `http` for readiness checks instead. |
| `contains "path" { ... }` | File contains a key (`format` = `"json"` or `"yaml"`); optionally binds to a local `var` |

### Condition Options

Any condition can have a sub-block with options:

| Option | Default | Description |
|--------|---------|-------------|
| `timeout` | `none` | Duration before giving up. `none` means wait indefinitely. |
| `poll` | `1s` (`100ms` for `after`) | Duration between checks |
| `retry` | `true` | `false` = fail immediately on first check |

The `status` option is specific to `http` conditions:

| Option | Default | Description |
|--------|---------|-------------|
| `status` | `200` | Expected HTTP status code |

```
wait {
  connect "127.0.0.1:5432" {
    timeout = 10s
    retry = false
  }
  after @migrate {
    timeout = 30s
  }
}
```

Use `timeout = none` to explicitly set an infinite wait.

### `var` Binding

The `contains` condition can extract a value into a job-scoped variable:

```
wait {
  contains "/tmp/config.yaml" {
    format = "yaml"
    key = "$.database.url"
    var = database_url
  }
}

env DB_URL = database_url
run "start-api --db $DB_URL"
```

The variable is scoped to the enclosing job (not to the `wait` block), so it can be referenced in `env` bindings and other expressions anywhere in the job body. It follows the same no-shadowing rules as `for` iteration variables — shadowing any existing name (args, other locals, other `var` bindings) is a parse-time error.

## Watches and Events

### Watch Blocks

Named runtime health checks that monitor a service after it starts:

```
service web {
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
```

| Field | Default | Description |
|-------|---------|-------------|
| check | (required) | One condition, same syntax as `wait` conditions |
| `initial_delay` | `0s` | Time before first check |
| `poll` | `5s` | Time between checks |
| `threshold` | `3` | Consecutive failures before triggering action |
| `on_fail` | `shutdown` | Action instruction |

### `on_fail` Actions (v1)

```
on_fail shutdown
on_fail debug
on_fail log
on_fail spawn @recovery
```

`on_fail` is a prefix to an action instruction, not an assignment. This leaves room for block-based multi-action handlers in the future.

### Event Handlers

Declared at the top level with `event`. Never auto-started:

```
event recovery {
  run "./scripts/recover.sh"
}
```

`on_fail spawn @name` must reference an `event`, not a `job` or `service`. The `@` sigil is a general "named entity" prefix used for jobs, services, and events throughout the language; the parser validates the target type based on context (`after` requires a `job`, `spawn` requires an event). When spawned, the event handler receives `PROCMAN_WATCH_*` environment variables with failure context.

## Expression Language

Expressions appear in `if` conditions, `env` value positions, and `var` bindings. Never evaluated inside shell strings.

### Value References

| Syntax | Description |
|--------|-------------|
| `args.name` | CLI arg value |
| `@job.KEY` | Output from a job's `PROCMAN_OUTPUT` |
| `local_var` | Job-scoped variable (from `for` or `var` binding) |

### Literals

| Type | Examples |
|------|---------|
| String | `"hello"`, `"3000"` |
| Number | `42`, `3.14` |
| Bool | `true`, `false` |
| Duration | `5s`, `500ms`, `2m` |
| None | `none` |

### Operators

| Category | Operators |
|----------|----------|
| Comparison | `==`, `!=`, `>`, `<`, `>=`, `<=` |
| Logical | `&&`, `\|\|`, `!` |
| Grouping | `( )` |

No arithmetic in v1.

### PROCMAN_OUTPUT Format

Every job and service receives a `PROCMAN_OUTPUT` environment variable pointing to a per-process output file. Jobs write key-value data to this file, which other jobs and services reference via `@job.KEY` expressions.

**Simple key-value lines:** `KEY=VALUE` (one per line, first `=` splits key from value).

**Heredoc blocks** for multi-line values:
```
CERT<<EOF
-----BEGIN CERTIFICATE-----
MIIBxTCCAWugAwIBAgIJALP...
-----END CERTIFICATE-----
EOF
```

The heredoc delimiter is arbitrary — `KEY<<DELIM` starts a block and a line containing only `DELIM` ends it.

### Type Errors

Type errors in expressions cause immediate procman runtime panic and shutdown. There is no silent coercion. A type error is a bug in the config.

## Validation

### Error Reporting

All parse-time and runtime errors include the source file path, line number, and column number (1-based) where the error was detected. Format: `{path}:{line}:{col}: {message}`.

### Parse-Time

- Syntax errors
- Duplicate job, service, or event names
- Jobs and services share a namespace — a service cannot have the same name as a job
- Duplicate watch names within a single job/service/event
- Unknown identifiers (referencing an arg or job that doesn't exist)
- `after @name` must target a `job` (not a `service`)
- `@job.KEY` references must point to a `job` (not a `service`)
- `@job.KEY` references require `after @job` in the referencing process's `wait` block (direct or transitive)
- Circular dependencies in `after` references
- `on_fail spawn @name` must reference an `event`
- Variable shadowing (between `for` loop variables and `contains` `var` bindings)
- Empty `run` commands

### Runtime

All fatal — immediate shutdown:

- Type errors in expression evaluation
- Missing key in `@job.KEY` resolution
- `glob()` pattern matching zero files
- Dependency timeout exceeded
- Non-zero exit from a job

**General principle:** All expressions in `.pman` files are evaluated at runtime, not parse time. The parser validates syntax, identifiers, and structural rules. Value resolution (including `glob()`, `@job.KEY`, and `args.*` references) happens at the point of use — after upstream dependencies are satisfied.

## Future Work (Out of Scope for v1)

- `include` / `import` for splitting configs across files
- `on_fail` block syntax for multi-action handlers
- Arithmetic in expressions

## Full Example

~~~
config {
  logs = "./my-logs"
  log_time = true
}

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

job migrate {
  run """
    ./run-migrations
    echo "DATABASE_URL=postgres://localhost:5432/mydb" > $PROCMAN_OUTPUT
  """
}

service web {
  env PORT = args.port
  run "serve --port $PORT"
}

service api {
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

service db {
  wait {
    connect "127.0.0.1:5432"
  }
  run "db-client start"
}

job extract-config {
  wait {
    contains "/tmp/config.json" {
      format = "json"
      key = "$.database.host"
      var = db_host
    }
  }
  env DB_HOST = db_host
  run "echo connected to $DB_HOST"
}

service healthcheck {
  wait {
    !connect "127.0.0.1:8080"
    !exists "/tmp/api.lock"
    !running "old-api.*"
  }
  run "api-server --port 8080"
}

service worker if args.enable_worker {
  run "worker-service start"
}

job nodes {
  for config_path in glob("/etc/nodes/*.yaml") {
    env NODE_CONFIG = config_path
    run "node-agent --config $NODE_CONFIG"
  }
}

service web-watched {
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
