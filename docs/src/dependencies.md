# Dependencies

The `wait` block controls process startup order. It contains conditions evaluated
sequentially — all must pass before `run` executes. A job without a `wait` block
starts immediately.

## Full Example

```
job api {
  env DB_URL = @migrate.DATABASE_URL

  wait {
    after @migrate
    http "http://localhost:3000/health" {
      status = 200
      timeout = 30s
      poll = 500ms
    }
    connect "127.0.0.1:5432"
    exists "/tmp/ready.flag"
  }

  run "api-server start --db $DB_URL"
}
```

Here `api` waits for `migrate` to exit, then checks an HTTP endpoint, then waits
for a TCP port, then checks for a file — all in order — before starting.

## Condition Types

### `after @job`

Wait for a `once = true` job to exit successfully (exit code 0).

```
wait {
  after @migrate
}
```

A non-zero exit triggers supervisor shutdown and the condition is never satisfied.
Parse-time error if the target job is not `once = true`.

For `for` jobs, `after @nodes` is satisfied only when **all** fan-out instances
have exited successfully.

### `http "url" { status = N }`

Wait for an HTTP endpoint to return an expected status code.

```
wait {
  http "http://localhost:8080/health" {
    status = 200
  }
}
```

The HTTP client uses a 5-second per-request timeout. Only the status code is
checked — the response body is ignored.

### `connect "host:port"`

Wait for a TCP port to accept connections.

```
wait {
  connect "127.0.0.1:5432"
}
```

Each poll attempt uses a 1-second connect timeout.

### `!connect "host:port"`

Wait until a TCP port is **not** accepting connections.

```
wait {
  !connect "127.0.0.1:8080"
}
```

The condition is satisfied when the connection is **refused** (nobody is
listening). Useful to ensure a stale process has released a port before starting
a replacement.

### `exists "path"`

Wait for a file to appear on disk.

```
wait {
  exists "/tmp/ready.flag"
}
```

### `!exists "path"`

Wait until a file does **not** exist on disk.

```
wait {
  !exists "/tmp/api.lock"
}
```

Useful to wait for a lockfile or PID file to be cleaned up.

### `!running "pattern"`

Wait until no process matching a pattern is running.

```
wait {
  !running "old-api.*"
}
```

Uses `pgrep -f` which matches against the full command line. Available on both
macOS and Linux. There is no positive `running` form — "wait until a process is
running" is inherently racy; use `connect` or `http` for readiness checks
instead.

### `contains "path" { ... }`

Wait for a file to contain a specific key, with optional value extraction into
a job-scoped variable.

```
wait {
  contains "/tmp/config.yaml" {
    format = "yaml"
    key = "$.database.url"
    var = database_url
  }
}
```

| Field | Required | Description |
|-------|----------|-------------|
| `format` | yes | `"json"` or `"yaml"` |
| `key` | yes | JSONPath expression ([RFC 9535](https://www.rfc-editor.org/rfc/rfc9535)) |
| `var` | no | If set, the resolved value is bound to this job-scoped variable |

The `key` field accepts a JSONPath expression. Use `$` to refer to the document
root, `.` to traverse nested maps, and bracket notation for array filtering. The
first matching value is used. Scalar values (strings, numbers, booleans) are
converted to strings. Null values are treated as missing. Mappings and sequences
are serialized as JSON strings.

**Array filtering example** — extract `rpc` from the entry where
`alias == "local"`:

```
wait {
  contains "/tmp/sui_client.yaml" {
    format = "yaml"
    key = "$.envs[?(@.alias == 'local')].rpc"
    var = sui_rpc_url
  }
}

env SUI_RPC_URL = sui_rpc_url
```

## Condition Options

Any condition can have a sub-block with options:

| Option | Default | Description |
|--------|---------|-------------|
| `timeout` | `60s` | Duration before giving up |
| `poll` | `1s` | Duration between checks |
| `retry` | `true` | `false` = fail immediately on first check |

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

Use `timeout = none` for infinite wait — useful for long-running setup tasks
where 60 seconds isn't enough.

### No-retry mode

When `retry = false`, the condition is checked exactly once. If it is not
satisfied on the first check, procman logs
`dependency failed (retry disabled): <description>` and triggers shutdown
immediately, without polling or waiting for a timeout.

This is useful to catch stale state that should have been cleaned up before
procman started: leftover lock files, ports still bound by a previous run, or
zombie processes.

```
wait {
  !exists "/tmp/api.lock" {
    retry = false
  }
}
```

## `var` Binding

The `contains` condition can extract a value into a job-scoped variable,
referenced in `env` bindings. The two-step pattern:

```
job api {
  wait {
    contains "/tmp/config.yaml" {
      format = "yaml"
      key = "$.database.url"
      var = database_url
    }
  }

  env DB_URL = database_url
  run "start-api --db $DB_URL"
}
```

The variable is scoped to the enclosing job (not to the `wait` block), so it can
be referenced in `env` bindings anywhere in the job body. It follows the same
no-shadowing rules as `for` iteration variables — shadowing any existing name
(args, other locals, other `var` bindings) is a parse-time error.

Note: `var` bindings are procman expressions, not direct env injections. They
enter the environment only when explicitly assigned via `env KEY = var_name`.

## Evaluation Order

Conditions within a `wait` block are evaluated **sequentially in declaration
order**. Each condition is fully satisfied before the next one is checked:

1. Start with the first condition.
2. Poll the current condition using its check function.
3. If the check succeeds, log `dependency satisfied: <description>` and advance
   to the next condition.
4. If the check fails for the first time and `retry` is `false`, log
   `dependency failed (retry disabled): <description>` and trigger shutdown.
5. Otherwise, if the check fails for the first time, log
   `dependency not ready: <description>` (logged only once per condition to
   avoid noise).
6. If the check fails, sleep for the condition's `poll` interval and retry.
7. Once all conditions are satisfied, proceed to spawn the process.

This sequential evaluation prevents stale-data races — for example, a `contains`
condition listed after an `after` condition will not be checked until the
upstream job has actually exited, ensuring it reads freshly generated data rather
than leftovers from a prior run.

## Timeout Behavior

Each condition's timeout clock starts when that condition begins being evaluated
(i.e., when the previous condition is satisfied), not when the waiter thread
starts. This means total wall-clock time for a job with multiple conditions is
the sum of individual wait times rather than the maximum.

If any single condition exceeds its timeout:

1. The waiter logs `dependency timed out: <description>`.
2. The global shutdown flag is set.
3. All processes are torn down (SIGTERM, then SIGKILL after a grace period).

A timed-out condition is fatal — procman does not continue with partial
dependencies.

## Circular Dependency Detection

At parse time, procman builds a directed graph from `after` references and runs
a DFS cycle detection pass. If a cycle is found, parsing fails with an error
showing the full cycle path:

```
Error: circular dependency: a -> b -> c -> a
```

Self-dependencies (`a -> a`) are also detected. References to job names not
defined in the config file are rejected with:

```
Error: process 'a' depends on unknown process 'nonexistent'
```
