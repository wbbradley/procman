# Configuration Reference

Procman reads a single YAML file (default `procman.yaml`) where each top-level key defines a
process. This chapter covers every field in detail.

## Top-level structure

The config file is a YAML map of **process-name → process definition**:

```yaml
web:
  run: ./start-web --port 8080

worker:
  env:
    RUST_LOG: debug
  run: cargo run --bin worker
  depends:
    - url: http://localhost:8080/health
      code: 200
```

Process names become the labels used in log output, dependency references, and template
expressions.

## Fields

### `run` (required)

The command to execute. How the command is executed depends on whether it is single-line or
multi-line:

**Single-line commands** are tokenized with POSIX shell quoting rules (via `shell_words`) and
exec'd directly — no shell is involved:

```yaml
api:
  run: cargo run --release --bin api-server
```

**Multi-line commands** (using YAML's `|` literal block scalar, which preserves newlines) are
passed to `sh -c` as a script. Shell features like pipes, redirects, `&&`, and variable
expansion work naturally:

```yaml
migrate:
  run: |
    ./run-migrations
    echo "DATABASE_URL=postgres://localhost:5432/mydb" > $PROCMAN_OUTPUT
```

> **Tip:** YAML's `>` (folded block scalar) joins lines with spaces, producing a single-line
> command that is tokenized and exec'd directly — it does **not** invoke `sh`.

The `run` field also supports [template references](templates.md) (`${{ process.key }}`). When
templates are present, shell quoting validation is deferred until after template resolution at
spawn time.

An empty or whitespace-only `run` value is rejected at parse time.

### `env` (optional)

A map of extra environment variables merged into the process's environment. The OS environment
is inherited first, then these values are layered on top (overriding any collisions).

```yaml
worker:
  env:
    RUST_LOG: debug
    PORT: "3000"
  run: my-server --port 3000
```

Values may contain [template references](templates.md):

```yaml
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
migrate:
  run: ./run-migrations
  once: true
```

### `depends` (optional)

A list of [dependency](dependencies.md) objects that must all be satisfied before the process
is started. See the [Dependencies](dependencies.md) chapter for the full reference.

```yaml
api:
  depends:
    - url: http://localhost:8080/health
      code: 200
    - process_exited: migrate
  run: ./start-api
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

## Environment variable expansion

Dependency paths (for `url`, `tcp`, `path`, and `file_contains.path` fields) support
environment variable expansion at parse time:

| Syntax | Behavior |
|--------|----------|
| `$VAR` | Replaced with the value of `VAR` |
| `${VAR}` | Replaced with the value of `VAR` (braced form) |
| `$$` | Escaped literal `$` |

If a referenced variable is not set, the expression is left unchanged (e.g. `$UNKNOWN`
remains `$UNKNOWN`).

```yaml
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

- **Non-empty `run`**: every process must have a non-empty run command.
- **Shell quoting**: single-line run commands without template references are parsed with
  `shell_words` to catch unterminated quotes. Multi-line commands skip this check (they are
  only validated for non-empty content).
- **Dependency graph cycles**: `process_exited` dependencies are checked for circular
  references using a DFS traversal. The error message shows the full cycle path
  (e.g. `circular dependency: a -> b -> c -> a`).
- **Unknown dependencies**: a `process_exited` dependency referencing a process name not
  defined in the config is rejected.
- **Template validation**: template references (`${{ process.key }}`) are checked against three
  rules — see [Templates](templates.md) for details.
