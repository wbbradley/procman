# Configuration Reference

Procman reads a single `.pman` file (passed as a positional argument). The file contains
top-level blocks in any order.

## Top-Level Structure

```
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
}

job migrate {
  once = true
  run "db-migrate up"
}

job web {
  env PORT = args.port
  run "serve --port $PORT"
}

job worker if args.enable_worker {
  run "worker-service start"
}

event recovery {
  run "./scripts/recover.sh"
}
```

A `.pman` file may contain:

- **`config { }`** — global settings, CLI args, and shared environment variables
- **`job name { }`** — an auto-started process (long-running or one-shot)
- **`job name if expr { }`** — a conditionally evaluated job
- **`event name { }`** — a dormant process, only started via `on_fail spawn`

## Identifiers

Job names, event names, arg names, and variable names are identifiers. Valid
identifiers match `[a-zA-Z_][a-zA-Z0-9_-]*` — they start with a letter or
underscore, followed by letters, digits, underscores, or hyphens.

## String Literals

String literals are double-quoted. Supported escape sequences: `\"` (literal
quote), `\\` (literal backslash), `\n` (newline), `\t` (tab). No other
backslash escapes are recognized.

## Duration Literals

Duration literals are a number followed by a unit suffix: `s` (seconds), `ms`
(milliseconds), `m` (minutes). Fractional values are allowed (e.g., `1.5s`).

## The `none` Literal

`none` represents the absence of a value. It is valid only in specific
positions: `timeout = none` (infinite wait), `default = none` (no default).
Using `none` in env value positions or boolean contexts is a parse-time error.

## `config { }` Block

Global settings applied to all jobs.

### `config.logs`

Optional log directory path. Defaults to `logs`. Recreated each run.

```
config {
  logs = "./my-logs"
}
```

### `config.env`

Global environment variable bindings applied to all jobs. Overridable per-job.

```
config {
  env {
    RUST_LOG = args.log_level
  }
}
```

### `config.arg`

CLI arguments parsed after `--`. Underscores become dashes on the CLI
(`log_level` → `--log-level`).

```
config {
  arg port {
    type = string
    default = "3000"
    short = "p"
    description = "Port to listen on"
  }

  arg enable_feature {
    type = bool
    default = false
  }
}
```

| Field | Required | Default | Description |
|-------|----------|---------|-------------|
| `type` | no | `string` | `string` or `bool` |
| `short` | no | — | Single character shorthand |
| `description` | no | — | Help text for `-- --help` |
| `default` | no | — | Fallback value. Args without a default are required. |

Arg values are referenced in expressions as `args.name`. There is no `env`
field on args — use `config { env { } }` to explicitly bind args to
environment variables.

Running `procman myapp.pman -- --help` prints generated usage based on the
arg definitions.

### Env Precedence

Lowest to highest:

| Source | Priority |
|--------|----------|
| System env (inherited) | lowest |
| CLI `-e KEY=VALUE` flags | |
| Global `config { env { } }` | |
| Per-job `env` | |
| Per-iteration `for` bindings | highest |

Note: `var` bindings from `contains` conditions are procman expressions, not
direct env injections. They enter the environment only when explicitly assigned
via `env KEY = var_name`.

## `job name { }` Block

Each `job` block defines a process to run.

### `run` (required)

The command to execute. All commands are passed to `sh -euo pipefail -c`, so
shell features like pipes, redirects, `&&`, variable expansion, and multi-line
scripts all work naturally.

Inline form:

```
run "echo hello"
```

Multi-line fenced form:

````
run ```
  ./run-migrations
  echo "DATABASE_URL=postgres://localhost:5432/mydb" > $PROCMAN_OUTPUT
```
````

Procman never interpolates inside shell strings. Values flow in exclusively
via environment variables.

An empty or whitespace-only `run` value is rejected at parse time.

### `env` (optional)

Environment variables merged into the job's environment. Single-binding and
block forms can coexist:

```
job api {
  env DB_URL = @migrate.DATABASE_URL
  env {
    API_KEY = "secret"
    LOG_DIR = args.log_dir
  }
  run "start-api --db $DB_URL"
}
```

### `once` (optional)

`once = true` marks a job as one-shot. Exit code 0 is treated as success and
does **not** trigger supervisor shutdown. A non-zero exit code still triggers
shutdown.

One-shot jobs can write key-value output to `$PROCMAN_OUTPUT`, which other
jobs reference via `@job.KEY` expressions (see [Process Output](templates.md)).

### `wait` (optional)

A block of conditions that must all be satisfied before `run` executes. See
the [Dependencies](dependencies.md) chapter for the full reference.

```
wait {
  after @migrate
  http "http://localhost:3000/health" {
    status = 200
    timeout = 30s
    poll = 500ms
  }
}
```

### `if` (optional)

An expression on the `job` line. If falsy, the job is not evaluated at all —
no dependency waiting, no env resolution. Skipped `once = true` jobs still
register as exited so `after` dependents can proceed.

```
job worker if args.enable_worker {
  run "worker-service start"
}
```

### `watch` (optional)

Named runtime health check blocks that monitor a job after it starts. See the
[Dependencies](dependencies.md) chapter for condition syntax.

```
job web {
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
}
```

### `for` (optional)

Iteration block that wraps `env` and `run`, spawning one instance per element.
See the [Fan-out](fan-out.md) chapter for full details.

```
job nodes {
  once = true
  for config_path in glob("configs/node-*.yaml") {
    env NODE_CONFIG = config_path
    run "start-node --config $NODE_CONFIG"
  }
}
```

## `event name { }` Block

Event handlers are declared at the top level. They are never auto-started —
they are spawned via `on_fail spawn @name` in a watch block.

```
event recovery {
  run "./scripts/recover.sh"
}
```

`on_fail spawn @name` must reference an `event`, not a `job`.

## Shell Blocks

Procman never interpolates inside shell strings. Values flow into shell
exclusively via environment variables set with `env` bindings.

Inline:
```
run "echo hello"
```

Multi-line fenced:
````
run ```
  ./run-migrations
  echo "DATABASE_URL=postgres://localhost:5432/mydb" > $PROCMAN_OUTPUT
```
````

## Expression Language

Expressions appear in `if` conditions, `env` value positions, and `var`
bindings. They are never evaluated inside shell strings.

### Value References

| Syntax | Description |
|--------|-------------|
| `args.name` | CLI arg value |
| `@job.KEY` | Output from a `once = true` job's `PROCMAN_OUTPUT` |
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

### Type Errors

Type errors in expressions cause immediate procman runtime panic and shutdown.
There is no silent coercion. A type error is a bug in the config.

## Parse-Time Validation

Procman validates the configuration at parse time and exits with an error
(with `file:line:col` location) if any of these checks fail:

- **Syntax errors** — malformed blocks, missing fields, invalid tokens
- **Unknown identifiers** — referencing an arg or job that doesn't exist
- **`after @job` targets** — must reference a `once = true` job
- **`@job.KEY` references** — must point to a `once = true` job and require
  `after @job` in the job's `wait` block (direct or transitive)
- **Circular dependencies** — cycles in `after` references
- **`on_fail spawn @name`** — must reference an `event`
- **Variable shadowing** — reusing a name already bound by `for`, `var`, or
  args
- **Empty `run` commands** — rejected at parse time
