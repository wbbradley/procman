# Process Output Templates

Templates let processes pass data to each other. A `once: true` process writes key-value
output, and downstream processes reference those values in their `run` command or `env` map.

## PROCMAN_OUTPUT

Every process receives a `PROCMAN_OUTPUT` environment variable pointing to a per-process output
file at `procman-logs/<name>.output`. Processes can write key-value data to this file, which
other processes can then read via template references.

## Output file format

The output file supports two formats:

**Simple key-value lines:**

```
DATABASE_URL=postgres://localhost:5432/mydb
API_KEY=secret123
```

**Heredoc blocks** for multi-line values:

```
CERT<<EOF
-----BEGIN CERTIFICATE-----
MIIBxTCCAWugAwIBAgIJALP...
-----END CERTIFICATE-----
EOF
```

The heredoc delimiter is arbitrary — `key<<DELIM` starts a block and a line containing only
`DELIM` ends it.

## Template syntax

Reference another process's output with `${{ process.key }}`:

```yaml
migrate:
  run: ./run-migrations
  once: true

api:
  depends:
    - process_exited: migrate
  env:
    DB_URL: "${{ migrate.DATABASE_URL }}"
  run: ./start-api --db "${{ migrate.DATABASE_URL }}"
```

Templates can appear in both `run` and `env` values. Multiple template references can appear
in a single string and can be mixed with literal text.

> **Tip:** In multi-line `run` scripts (executed via `sh -euo pipefail -c`), quote template references to
> protect against whitespace or special characters in resolved values:
>
> ```yaml
> run: |
>   echo "Connecting to ${{ migrate.DATABASE_URL }}"
>   exec ./start-api --db "${{ migrate.DATABASE_URL }}"
> ```

## Resolution

Template resolution happens **at spawn time**, after all dependencies for the process are
satisfied. The resolver:

1. Reads the referenced process's output file (`procman-logs/<process>.output`).
2. Parses it into a key-value map.
3. Substitutes each `${{ process.key }}` with the corresponding value.

If a referenced key is not found in the output file, resolution fails and the process is not
started.

## Validation rules

Procman enforces three rules at parse time to catch template errors before any process starts:

### Rule 1: Referenced process must exist

```yaml
app:
  run: echo ${{ nonexistent.KEY }}  # Error: process 'nonexistent' does not exist
```

### Rule 2: Referenced process must be `once: true`

Only `once: true` processes produce output that is guaranteed to be available. Referencing a
long-running process is rejected:

```yaml
server:
  run: ./start-server  # not once: true

app:
  run: echo ${{ server.PORT }}  # Error: process 'server' is not once: true
```

### Rule 3: Referencing process must depend on the referenced process

The referencing process must have a `process_exited` dependency (direct or transitive) on the
referenced process. This guarantees the output file exists when templates are resolved:

```yaml
setup:
  run: ./setup
  once: true

app:
  run: echo ${{ setup.KEY }}  # Error: no process_exited dependency on 'setup'
```

Transitive dependencies are followed — if `app` depends on `middle` and `middle` depends on
`setup`, then `app` can reference `setup`'s output.

## file_contains with env

The `file_contains` dependency type offers an alternative way to pass data between processes.
When the `env` field is specified, the value at the given key is extracted and injected as an
environment variable:

```yaml
setup:
  run: ./generate-config
  once: true

api:
  depends:
    - process_exited: setup
    - file_contains:
        path: procman-logs/setup.output
        format: yaml
        key: "$.database.url"
        env: DATABASE_URL
  run: ./start-api
```

This approach does not require template syntax — the value is available as a regular
environment variable (`$DATABASE_URL`).

## End-to-end example

A migration process writes a database URL, and the API server reads it via a template:

```yaml
migrate:
  run: |
    ./run-migrations
    echo "DATABASE_URL=postgres://localhost:5432/mydb" > $PROCMAN_OUTPUT
  once: true

api:
  depends:
    - process_exited: migrate
  env:
    DB_URL: "${{ migrate.DATABASE_URL }}"
  run: ./start-api --db "${{ migrate.DATABASE_URL }}"
```

The sequence:

1. `migrate` starts and runs migrations.
2. `migrate` writes `DATABASE_URL=postgres://...` to its `$PROCMAN_OUTPUT` file.
3. `migrate` exits with code 0 — procman marks it as complete.
4. `api`'s `process_exited: migrate` dependency is satisfied.
5. Procman resolves `${{ migrate.DATABASE_URL }}` by reading `migrate`'s output file.
6. `api` starts with `DB_URL` set in its environment and the URL substituted into its `run`
   command.
