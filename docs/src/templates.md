# Process Output

Every job receives a `PROCMAN_OUTPUT` environment variable pointing to a per-job
output file at `procman-logs/<name>.output`. One-shot (`once = true`) jobs write
data here; downstream jobs reference it with `@job.KEY` expressions.

## Output File Format

The output file supports two formats:

**Simple key-value lines** — one per line, first `=` splits key from value:

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

The heredoc delimiter is arbitrary — `KEY<<DELIM` starts a block and a line
containing only `DELIM` ends it.

## Referencing Output

Reference another job's output with `@job.KEY` in `env` bindings. Values flow
into shell via environment variables — procman never interpolates inside shell
strings.

```
job migrate {
  once = true
  run ```
    ./run-migrations
    echo "DATABASE_URL=postgres://localhost:5432/mydb" > $PROCMAN_OUTPUT
  ```
}

job api {
  env DB_URL = @migrate.DATABASE_URL

  wait {
    after @migrate
  }

  run "api-server --db $DB_URL"
}
```

The sequence:

1. `migrate` starts and runs migrations.
2. `migrate` writes `DATABASE_URL=postgres://...` to its `$PROCMAN_OUTPUT` file.
3. `migrate` exits with code 0 — procman marks it as complete.
4. `api`'s `after @migrate` condition is satisfied.
5. Procman resolves `@migrate.DATABASE_URL` by reading `migrate`'s output file.
6. `api` starts with `DB_URL` set in its environment.

## Resolution

Output resolution happens **at spawn time**, after all `wait` conditions for the
job are satisfied. The resolver:

1. Reads the referenced job's output file (`procman-logs/<job>.output`).
2. Parses it into a key-value map.
3. Substitutes each `@job.KEY` reference with the corresponding value.

If a referenced key is not found in the output file, resolution fails and the
job is not started.

## Validation Rules

Procman enforces three rules at parse time to catch output reference errors
before any job starts:

### Rule 1: Referenced job must exist

```
job app {
  env KEY = @nonexistent.KEY  # Error: job 'nonexistent' does not exist
  run "echo $KEY"
}
```

### Rule 2: Referenced job must be `once = true`

Only `once = true` jobs produce output that is guaranteed to be available.
Referencing a long-running job is rejected:

```
job server {
  run "start-server"  # not once = true
}

job app {
  env PORT = @server.PORT  # Error: job 'server' is not once = true
  run "echo $PORT"
}
```

### Rule 3: Referencing job must have `after @job` in its `wait` block

The referencing job must have an `after` condition (direct or transitive) on the
referenced job. This guarantees the output file exists when references are
resolved:

```
job setup {
  once = true
  run "echo KEY=value > $PROCMAN_OUTPUT"
}

job app {
  env KEY = @setup.KEY  # Error: no 'after @setup' in wait block
  run "echo $KEY"
}
```

Transitive dependencies are followed — if `app` waits on `middle` and `middle`
waits on `setup`, then `app` can reference `setup`'s output.
