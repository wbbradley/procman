# Process Output

Every job and service receives a `PROCMAN_OUTPUT` environment variable pointing
to a per-process output file at `logs/procman/<name>.output`. Jobs (one-shot
processes) write data here; downstream jobs and services reference it with
`@job.KEY` expressions.

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

~~~
job migrate {
  run """
    ./run-migrations
    echo "DATABASE_URL=postgres://localhost:5432/mydb" > $PROCMAN_OUTPUT
  """
}

service api {
  env DB_URL = @migrate.DATABASE_URL

  wait {
    after @migrate
  }

  run "api-server --db $DB_URL"
}
~~~

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

1. Reads the referenced job's output file (`logs/procman/<job>.output`).
2. Parses it into a key-value map.
3. Substitutes each `@job.KEY` reference with the corresponding value.

If a referenced key is not found in the output file, resolution fails and the
job is not started.

## Validation Rules

Procman enforces three rules at parse time to catch output reference errors
before any job starts:

### Rule 1: Referenced process must exist

```
job app {
  env KEY = @nonexistent.KEY  # Error: process 'nonexistent' does not exist
  run "echo $KEY"
}
```

### Rule 2: Referenced process must be a `job`

Only jobs (one-shot processes) produce output that is guaranteed to be available.
Referencing a service is rejected:

```
service server {
  run "start-server"
}

job app {
  env PORT = @server.PORT  # Error: 'server' is not a job
  run "echo $PORT"
}
```

### Rule 3: Referencing process must have `after @job` in its `wait` block

The referencing job or service must have an `after` condition (direct or
transitive) on the referenced job. This guarantees the output file exists when
references are resolved:

```
job setup {
  run "echo KEY=value > $PROCMAN_OUTPUT"
}

service app {
  env KEY = @setup.KEY  # Error: no 'after @setup' in wait block
  run "echo $KEY"
}
```

Transitive dependencies are followed — if `app` waits on `middle` and `middle`
waits on `setup`, then `app` can reference `setup`'s output.
