# Getting Started

## A Minimal Configuration

Create a file called `procman.yaml` in your project root:

```yaml
web:
  run: python3 -m http.server 8000

api:
  run: node server.js
```

Each top-level key is a process name, and `run` is the command to execute. That's
all you need.

## Running

Start everything with:

```sh
procman run
```

Or simply:

```sh
procman
```

The `run` subcommand is the default. procman reads `procman.yaml` from the
current directory, spawns both processes, and multiplexes their output:

```
   web | Serving HTTP on 0.0.0.0 port 8000
   api | Server listening on port 3000
   web | 127.0.0.1 - "GET / HTTP/1.1" 200 -
```

Process names are right-aligned and separated from output by a `|` character,
making it easy to scan which process produced each line.

## Log Files

procman automatically writes logs to a `procman-logs/` directory:

- `procman-logs/web.log` — output from the `web` process
- `procman-logs/api.log` — output from the `api` process
- `procman-logs/procman.log` — combined output from all processes

These files are created fresh on each run.

## Stopping

Press **Ctrl-C** to shut down. procman sends SIGTERM to every child process,
waits up to 2 seconds for them to exit, then sends SIGKILL to anything still
running. procman exits with the exit code of the first process that terminated.

## A More Advanced Example

Here's a configuration that uses `once` processes and dependencies:

```yaml
migrate:
  run: db-migrate up
  once: true

web:
  env:
    PORT: "3000"
  run: serve --port $PORT

api:
  depends:
    - process_exited: migrate
    - url: http://localhost:3000/health
      code: 200
      poll_interval: 0.5
      timeout_seconds: 30
  run: api-server start
```

In this setup:

- **migrate** runs the database migration and exits. The `once: true` flag means
  its successful exit (code 0) won't trigger a shutdown of everything else.
- **web** starts immediately and serves on port 3000.
- **api** waits for two things before starting: the `migrate` process must have
  exited successfully, and the web server's health endpoint must be returning
  HTTP 200. Only then does `api-server start` run.

This is the core pattern for dependency-aware startup — later chapters cover the
full set of dependency types and more advanced features like fan-out and process
output templates.
