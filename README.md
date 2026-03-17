# procman

[![crates.io](https://img.shields.io/crates/v/procman.svg)](https://crates.io/crates/procman)

A foreman-like process supervisor written in Rust. Reads a `Procfile`, spawns all listed commands, multiplexes their output with name prefixes, and tears everything down cleanly when any child exits or a signal arrives.

## Usage

```
cargo install --path .
procman [Procfile]
```

Defaults to `Procfile` in the current directory if no path is given.

## Procfile Format

```
# Global environment variables (before any command lines)
DATABASE_URL=postgres://localhost/myapp
PORT=3000

# Commands — one per line
web serve --port $PORT
worker process-jobs --db $DATABASE_URL
```

- Lines starting with `#` are comments.
- Trailing `\` joins continuation lines.
- `KEY=value` lines before the first command set global environment variables.
- Inline `KEY=value` tokens at the start of a command line set per-command env vars.
- `$VAR` references are substituted from the merged environment (inherited + global + inline). Undefined variables are a hard error — nothing is spawned.
- Process names are derived from the program basename. Duplicates get `.1`, `.2` suffixes.

## Behavior

- All children share a process group.
- stderr is merged into stdout per-process.
- Output is prefixed with the process name, right-aligned and padded.
- Per-process logs are written to `./logs/<name>.log`.
- On SIGINT or SIGTERM, all children receive SIGTERM. After a 2-second grace period, remaining processes are sent SIGKILL.
- procman exits with the first child's exit code.

## License

MIT
