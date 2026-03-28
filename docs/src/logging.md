# Logging & Output

Procman captures all process output and writes it to both the terminal and log files.

## Multiplexed stdout

All process output is interleaved on procman's stdout with right-aligned name labels and a `|`
separator:

```
 procman | started with 3 process(es), mode=run
     web | listening on :8080
  worker | processing jobs
     web | GET /health 200
```

The name column width adjusts to the longest process name so that all `|` separators align.

## Log directory

At startup, procman creates (or recreates) a `logs/procman/` directory in the current working
directory. Any existing `logs/procman/` directory is removed first to ensure a clean state.

## Combined log

`logs/procman/procman.log` contains every line from every process, in the same right-aligned
format as the terminal output. This is a complete record of the session.

## Per-process logs

Each process gets its own log file at `logs/procman/<name>.log`. These files contain only that
process's output lines with **no name prefix** — just the raw output. This makes them easy to
feed into other tools or search with `grep`.

The `procman` pseudo-process does not get its own per-process log file. Supervisor messages
appear only in the combined log and on stdout.

## Supervisor messages

Procman logs its own messages under the `procman` name. These include:

- Startup information (process count, mode).
- Dependency status (waiting, satisfied).
- Process lifecycle events (started, completed, exited, killed).
- Shutdown sequence progress.
- Error messages.

## stderr handling

Each child process has stderr redirected to stdout (`dup2`) before exec. This means both
streams are captured through the same pipe and appear interleaved in the logs. There is no
separate stderr log.

