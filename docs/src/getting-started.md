# Getting Started

## A Minimal Configuration

Create a file called `procman.pman` in your project root:

```
service web {
  run "python3 -m http.server 8000"
}

service api {
  run "node server.js"
}
```

Each `service` block defines a long-running process with a name and a `run`
command. That's all you need.

## Running

Start everything with:

```sh
procman procman.pman
```

The config file path is a required positional argument. procman spawns both
processes and multiplexes their output:

```
   web | Serving HTTP on 0.0.0.0 port 8000
   api | Server listening on port 3000
   web | 127.0.0.1 - "GET / HTTP/1.1" 200 -
```

Process names are right-aligned and separated from output by a `|` character,
making it easy to scan which process produced each line.

## Log Files

procman automatically writes logs to a `logs/procman/` directory:

- `logs/procman/web.log` — output from the `web` service
- `logs/procman/api.log` — output from the `api` service
- `logs/procman/procman.log` — combined output from all processes

These files are created fresh on each run.

## Stopping

Press **Ctrl-C** to shut down. procman sends SIGTERM to every child process,
waits up to 2 seconds for them to exit, then sends SIGKILL to anything still
running. procman exits with the exit code of the first process that terminated.

## A More Advanced Example

Here's a configuration that uses jobs, services, dependencies, and environment
variables:

```
job migrate {
  run "db-migrate up"
}

service web {
  env PORT = "3000"
  run "serve --port $PORT"
}

service api {
  wait {
    after @migrate
    http "http://localhost:3000/health" {
      status = 200
      poll = 500ms
      timeout = 30s
    }
  }
  run "api-server start"
}
```

In this setup:

- **migrate** is a `job` that runs the database migration and exits. Jobs run
  to completion — a successful exit (code 0) won't trigger a shutdown of
  everything else.
- **web** is a `service` that starts immediately and serves on port 3000, with
  the `PORT` environment variable set via `env`.
- **api** is a `service` that waits for two things before starting: the
  `migrate` job must have exited successfully (`after @migrate`), and the web
  server's health endpoint must be returning HTTP 200. Only then does
  `api-server start` run.

This is the core pattern for dependency-aware startup — later chapters cover the
full set of dependency types and more advanced features like fan-out and process
output.
