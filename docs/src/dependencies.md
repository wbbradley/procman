# Dependencies

Dependencies let you control process startup order. Each dependency is polled in a loop until
it is satisfied or its timeout expires. A process is not started until **all** of its
dependencies are met.

## Dependency types

### HTTP health check

Wait for an HTTP endpoint to return an expected status code.

```yaml
depends:
  - url: http://localhost:8080/health
    code: 200
```

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `url` | string | yes | — | URL to GET |
| `code` | integer | yes | — | Expected HTTP status code |
| `poll_interval` | float | no | 1.0 | Seconds between polls |
| `timeout_seconds` | integer | no | 60 | Seconds before giving up |

The HTTP client uses a 5-second per-request timeout. Only the status code is checked — the
response body is ignored.

### TCP connect

Wait for a TCP port to accept connections.

```yaml
depends:
  - tcp: "127.0.0.1:5432"
```

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `tcp` | string | yes | — | Address in `host:port` form |
| `poll_interval` | float | no | 1.0 | Seconds between polls |
| `timeout_seconds` | integer | no | 60 | Seconds before giving up |

Each poll attempt uses a 1-second connect timeout.

### File exists

Wait for a file to appear on disk.

```yaml
depends:
  - path: /tmp/ready.flag
```

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `path` | string | yes | — | Path to check |

Poll interval is 1 second. Timeout is 60 seconds. These are not configurable for this
dependency type.

### File contains key

Wait for a file to contain a specific key, with optional value extraction into the process
environment.

```yaml
depends:
  - file_contains:
      path: /tmp/config.yaml
      format: yaml
      key: database.url
      env: DATABASE_URL
```

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `path` | string | yes | — | Path to the file |
| `format` | string | yes | — | `"json"` or `"yaml"` |
| `key` | string | yes | — | Dot-separated path to look up (e.g. `database.url`) |
| `env` | string | no | — | If set, the resolved value is injected as this env var |
| `poll_interval` | float | no | 1.0 | Seconds between polls |
| `timeout_seconds` | integer | no | 60 | Seconds before giving up |

Key lookup traverses nested maps using `.` as a separator. Scalar values (strings, numbers,
booleans) are converted to strings. Null values are treated as missing. Mappings and sequences
are serialized as JSON strings.

When `env` is specified, the value at `key` is extracted and injected into the dependent
process's environment under that variable name. This happens after all dependencies are
satisfied, just before the process is spawned. See [Templates](templates.md) for more on
passing data between processes.

### Process exited

Wait for another process to exit successfully (`once: true` processes only, in practice).

```yaml
depends:
  - process_exited: migrate
```

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `process_exited` | string | yes | — | Name of the process to wait for |

Poll interval is 100ms. Timeout is 60 seconds. These are not configurable for this
dependency type.

This dependency is satisfied when the named process has exited (with any exit code). It is
typically used with `once: true` processes like migrations or setup scripts.

For `for_each` processes, a `process_exited` dependency on the template name is satisfied
only when **all** fan-out instances have exited.

## How polling works

When a process has dependencies, procman spawns a dedicated waiter thread that loops over all
unsatisfied dependencies:

1. For each unsatisfied dependency, call the check function.
2. If a check succeeds, mark it satisfied and log `dependency satisfied: <description>`.
3. If a check fails for the first time, log `dependency not ready: <description>` (logged
   only once per dependency to avoid noise).
4. If all dependencies are satisfied, proceed to spawn the process.
5. Otherwise, sleep for the shortest `poll_interval` among unsatisfied dependencies and loop.

## Timeout behavior

If any single dependency exceeds its timeout:

1. The waiter logs `dependency timed out: <description>`.
2. The global shutdown flag is set.
3. All processes are torn down (SIGTERM, then SIGKILL after a grace period).

A timed-out dependency is fatal — procman does not continue with partial dependencies.

## Circular dependency detection

At parse time, procman builds a directed graph from `process_exited` dependencies and runs a
DFS cycle detection pass. If a cycle is found, parsing fails with an error showing the full
cycle path:

```
Error: circular dependency: a -> b -> c -> a
```

Self-dependencies (`a -> a`) are also detected. References to process names not defined in the
config file are rejected with:

```
Error: process 'a' depends on unknown process 'nonexistent'
```

## Environment variable expansion in paths

The `url`, `tcp`, `path`, and `file_contains.path` fields support environment variable
expansion at parse time. See the [Configuration](configuration.md#environment-variable-expansion)
chapter for the full syntax.

```yaml
depends:
  - path: $HOME/.config/app/ready.flag
  - tcp: "${DB_HOST}:5432"
```
