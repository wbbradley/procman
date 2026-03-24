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
| `retry` | bool | no | true | If false, fail immediately on first check instead of polling |

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
| `retry` | bool | no | true | If false, fail immediately on first check instead of polling |

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
| `retry` | bool | no | true | If false, fail immediately on first check instead of polling |

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
      key: "$.database.url"
      env: DATABASE_URL
```

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `path` | string | yes | — | Path to the file |
| `format` | string | yes | — | `"json"` or `"yaml"` |
| `key` | string | yes | — | JSONPath expression ([RFC 9535](https://www.rfc-editor.org/rfc/rfc9535)) |
| `env` | string | no | — | If set, the resolved value is injected as this env var |
| `poll_interval` | float | no | 1.0 | Seconds between polls |
| `timeout_seconds` | integer | no | 60 | Seconds before giving up |
| `retry` | bool | no | true | If false, fail immediately on first check instead of polling |

The `key` field accepts a JSONPath expression (RFC 9535). Use `$` to refer to the document root,
`.` to traverse nested maps, and bracket notation for array filtering. The first matching value
is used. Scalar values (strings, numbers, booleans) are converted to strings. Null values are
treated as missing. Mappings and sequences are serialized as JSON strings.

**Array filtering example** — extract `rpc` from the entry where `alias == "local"`:

```yaml
depends:
  - file_contains:
      path: /tmp/sui_client.yaml
      format: yaml
      key: "$.envs[?(@.alias == 'local')].rpc"
      env: SUI_RPC_URL
```

When `env` is specified, the value at `key` is extracted and injected into the dependent
process's environment under that variable name. This happens after all dependencies are
satisfied, just before the process is spawned. See [Templates](templates.md) for more on
passing data between processes.

### Process exited

Wait for another process to exit successfully (`once: true` processes only, in practice).

**Simple string form** — uses a 60-second default timeout:

```yaml
depends:
  - process_exited: migrate
```

**Expanded object form** — allows timeout control:

```yaml
depends:
  - process_exited:
      name: migrate
      timeout_seconds: 30
```

**Infinite wait** — use `timeout_seconds: null` to wait indefinitely:

```yaml
depends:
  - process_exited:
      name: migrate
      timeout_seconds: null
```

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `process_exited` | string or object | yes | — | Process name (string) or object with `name` and `timeout_seconds` |
| `process_exited.name` | string | yes (object form) | — | Name of the process to wait for |
| `process_exited.timeout_seconds` | integer or null | no | 60 | Seconds before giving up. `null` waits indefinitely. |
| `retry` | bool | no | true | If false, fail immediately on first check instead of polling |

Poll interval is 100ms. This is not configurable for this dependency type.

This dependency is satisfied when the named process has exited successfully (exit code 0).
A non-zero exit triggers supervisor shutdown and the dependency is never satisfied. This
only works with `once: true` processes (e.g. migrations or setup scripts).

Use the simple string form for most cases. Use the expanded form when you need a shorter
timeout (to fail fast) or an infinite wait (for long-running setup tasks where 60 seconds
isn't enough).

For `for_each` processes, a `process_exited` dependency on the template name is satisfied
only when **all** fan-out instances have exited.

### TCP not listening

Wait until a TCP port is **not** accepting connections.

```yaml
depends:
  - not_listening: "127.0.0.1:8080"
```

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `not_listening` | string | yes | — | Address in `host:port` form |
| `poll_interval` | float | no | 1.0 | Seconds between polls |
| `timeout_seconds` | integer | no | 60 | Seconds before giving up |
| `retry` | bool | no | true | If false, fail immediately on first check instead of polling |

Each poll attempts a TCP connect with a 1-second timeout. The dependency is satisfied when the
connection is **refused** (nobody is listening). Useful to ensure a stale process has released a
port before starting a replacement.

### File not exists

Wait until a file does **not** exist on disk.

```yaml
depends:
  - not_exists: /tmp/api.lock
```

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `not_exists` | string | yes | — | Path to check |
| `retry` | bool | no | true | If false, fail immediately on first check instead of polling |

Poll interval is 1 second, timeout is 60 seconds (not configurable). Useful to wait for a
lockfile or PID file to be cleaned up.

### Process not running

Wait until no process matching a pattern is running.

```yaml
depends:
  - not_running: "old-api.*"
```

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `not_running` | string | yes | — | Pattern passed to `pgrep -f` |
| `retry` | bool | no | true | If false, fail immediately on first check instead of polling |

Uses `pgrep -f` which matches against the full command line. Available on both macOS and Linux.
Poll interval is 1 second, timeout is 60 seconds (not configurable). The dependency is satisfied
when `pgrep` finds no matching processes.

## No-retry mode

All dependency types accept an optional `retry` field (default `true`). When set to `false`,
the dependency is checked exactly once — if it is not satisfied on the first check, procman
logs `dependency failed (retry disabled): <description>` and triggers a shutdown immediately,
without polling or waiting for a timeout.

This is useful to catch stale state that should have been cleaned up before procman started:
leftover lock files, ports still bound by a previous run, or zombie processes. Rather than
silently waiting (and eventually timing out), `retry: false` makes the failure explicit and fast.

```yaml
depends:
  - not_exists: /tmp/api.lock
    retry: false
```

## How polling works

When a process has dependencies, procman spawns a dedicated waiter thread that evaluates
dependencies **in declaration order**. Each dependency is fully satisfied before the next one
is evaluated:

1. Start with the first dependency.
2. Poll the current dependency using its check function.
3. If the check succeeds, log `dependency satisfied: <description>` and advance to the next
   dependency.
4. If the check fails for the first time and `retry` is `false`, log
   `dependency failed (retry disabled): <description>` and trigger shutdown immediately.
5. Otherwise, if the check fails for the first time, log `dependency not ready: <description>`
   (logged only once per dependency to avoid noise).
6. If the check fails, sleep for the dependency's `poll_interval` and retry.
7. Once all dependencies are satisfied, proceed to spawn the process.

This sequential evaluation prevents stale-data races — for example, a `file_contains`
dependency listed after a `process_exited` dependency will not be checked until the process
has actually exited, ensuring it reads freshly generated data rather than leftovers from a
prior run.

## Timeout behavior

Each dependency's timeout clock starts when that dependency begins being evaluated (i.e., when
the previous dependency is satisfied), not when the waiter thread starts. This means total
wall-clock time for a process with multiple dependencies is the sum of individual wait times
rather than the maximum.

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

The `url`, `tcp`, `path`, `file_contains.path`, `not_listening`, `not_exists`, and `not_running`
fields support environment variable expansion at parse time. See the [Configuration](configuration.md#environment-variable-expansion)
chapter for the full syntax.

```yaml
depends:
  - path: $HOME/.config/app/ready.flag
  - tcp: "${DB_HOST}:5432"
```
