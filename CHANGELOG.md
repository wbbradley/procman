# Changelog

## [0.6.0] - 2026-03-18

### Breaking Changes
- FIFO wire protocol replaced with JSON. Direct FIFO writers (e.g., `echo "command" > /tmp/fifo`) must now send JSON messages (`{"type":"run","name":"...","run":"..."}`). The `procman start` CLI handles this transparently.

### Added
- `procman stop <FIFO>` subcommand for graceful remote shutdown of a running `procman serve` instance.
- `once: true` process mode. Run-once processes exit cleanly on success (code 0) without triggering supervisor shutdown. Non-zero exit still triggers shutdown.
- `process_exited` dependency type. Processes can depend on a `once: true` process completing successfully before starting (e.g., `depends: [{process_exited: migrate}]`).

### Removed
- Plain-text FIFO protocol and `CommandParser` module (replaced by JSON wire protocol).

## [0.5.1] - 2026-03-18

### Added
- Combined log file: all formatted log output (the `"name | line"` view shown on stdout) is now also written to `procman-logs/procman.log`, providing a single file with interleaved output from all processes.

### Changed
- The internal "procman" pseudo-process no longer produces a redundant per-process log file; its output is captured in the combined `procman.log` instead.

## [0.5.0] - 2026-03-18

### Breaking Changes
- Default config file is now `procman.yaml` (was `Procfile`).
- Removed legacy text Procfile format; only YAML config files are supported.
- Log directory moved from `./logs/` to `./procman-logs/` and is wiped clean at the start of each session.

### Changed
- CLI positional argument renamed from `procfile` to `config` in `run` and `serve` subcommands.

### Removed
- Legacy text Procfile parser and fallback logic.
- Legacy text format documentation from README.

## [0.4.0] - 2026-03-18

### Added
- YAML Procfile format with structured process definitions (`run`, `env`, `depends` fields). Auto-detected; falls back to legacy text format.
- Dependency checking and startup ordering: processes can declare `depends` with HTTP health check (`url` + `code`, optional `poll_interval`/`timeout`) or file-exists (`path`) conditions. Dependent processes are held until all dependencies are satisfied; a timeout triggers shutdown.
- Lifecycle logging via a `procman` pseudo-process: supervisor startup/shutdown, process spawn/exit events with PID and elapsed time, dependency status, and FIFO command submissions.

### Fixed
- Race condition in `FifoServer::stop()` where a single wake-up open could miss the reader thread between its shutdown check and blocking `File::open`, causing a hang. Now retries until the thread exits.
- Test temp directory collisions under parallel execution (replaced nanosecond timestamps with atomic counters).

## [0.3.1] - 2026-03-17

### Fixed
- Fix `procman serve` panic caused by a required positional argument (`fifo`) appearing after an optional one (`procfile`). The `serve` subcommand was unusable in 0.3.0.

### Changed
- `procman serve` now takes `<FIFO> [PROCFILE]` (required arg first) instead of the broken `[PROCFILE] <FIFO>` order from 0.3.0.
- Improved help text: clearer `serve` and `start` descriptions, added `SIGNALS` section documenting shutdown behavior.
- Updated examples to use unambiguous command (`redis-server --port 6380` instead of `worker process-jobs`).

## [0.3.0] - 2026-03-17

### Breaking Changes
- CLI: replaced `--server`/`--client` flags with `run`/`serve`/`start` subcommands. Scripts using `--server <FIFO>` must switch to `procman serve <FIFO>`. Scripts using `--client <FIFO> <CMD>` must switch to `procman start <FIFO> <CMD>`. Bare `procman` (no arguments) continues to work unchanged.

### Added
- Each subcommand has dedicated `--help` with contextual documentation
- Top-level `--help` includes an `EXAMPLES` section showing the scripted service bringup pattern

### Changed
- `serve` takes `procfile` as the first positional and `fifo` as the second (previously Procfile was top-level and FIFO was via `--server`)

### Removed
- `-s`/`--server` and `-c`/`--client` flags

## [0.2.0] - 2026-03-17

### Breaking Changes
- `procfile::parse()` now returns `(Procfile, CommandParser)` instead of just `Procfile`
- `ProcessGroup::wait_and_shutdown()` requires two additional parameters: `mpsc::Receiver<Command>` and `Arc<Mutex<Logger>>`
- Procfile command-line tokenization now uses POSIX shell quoting (`shell_words`) instead of naive whitespace splitting; quoted strings are handled correctly but Procfiles relying on the old literal behavior may parse differently

### Added
- Server mode (`--server` / `-s`): run with a named FIFO that accepts new process commands at runtime
- Client mode (`--client` / `-c`): send a command string to a running server via its FIFO
- Advisory `flock` on the Procfile to prevent multiple instances managing the same file
- `--version` flag via clap
- `CommandParser` type for parsing individual command lines into `Command` values
- `Logger::add_process()` for registering new process names after startup
- Unit and integration tests for FIFO server/client and Procfile parsing

### Changed
- CLI parsing switched from raw `std::env::args()` to clap derive API
- `ProcessGroup` supports dynamic process spawning via mpsc channel

## 0.1.0

- Initial release of procman, a foreman-like process supervisor
- Procfile parsing with `\`-continuation lines, `# comments`, and blank line stripping
- Global and inline `KEY=value` environment variable support
- `$VAR` substitution across env values, programs, and args (hard error on undefined)
- Process group management via `setpgid` with stderr merged into stdout
- Multiplexed, name-prefixed stdout output with right-aligned padding
- Per-process log files written to `./logs/`
- Graceful shutdown on SIGINT/SIGTERM: SIGTERM → 2s grace → SIGKILL
- Automatic process name deduplication with `.1`, `.2` suffixes
