# Changelog

## [0.10.1] - 2026-03-20

### Fixed
- Debug mode no longer lists already-exited once-processes as "still running." Previously, a fast `once: true` process could appear in the debug pause output even after completing successfully.
- Documentation corrections: `process_exited` dependency requires exit code 0 (not any code); undefined env vars produce an error (not pass-through); env var expansion field list now complete; `file_contains` key example uses JSONPath syntax.

## [0.10.0] - 2026-03-20

### Breaking Changes
- All `run` commands are now executed via `sh -c`, including single-line commands. Previously, single-line commands were tokenized with `shell_words` and exec'd directly. Parse-time shell quoting validation is removed; shell syntax errors are reported at runtime by `sh`.
- In `run` mode, procman now exits with code 0 when all `once` processes complete and no long-running processes remain. Previously it would hang indefinitely. `serve` mode is unaffected.

### Added
- Inverse dependency types: `not_listening` (TCP port not accepting connections), `not_exists` (file does not exist), `not_running` (no process matching pattern via `pgrep -f`). All support `poll_interval`, `timeout_seconds`, and `retry`.
- `retry` field on all dependency types (default `true`). Set `retry: false` to fail immediately on the first unsuccessful check instead of polling.

### Changed
- Default exit code when no child has exited is now `0` instead of `1`.

### Fixed
- Shell operators (`&&`, `|`, `;`, `>`) in single-line `run` commands now work correctly instead of being passed as literal arguments.

## [0.9.0] - 2026-03-19

### Added
- `--debug` flag for `run` and `serve` subcommands: pauses shutdown when a child process fails, printing which process triggered the shutdown and which are still running, then waiting for ENTER or Ctrl+C before proceeding. Requires an interactive terminal (TTY).

### Changed
- Dependencies are now evaluated sequentially in declaration order. Each dependency must be fully satisfied before the next is checked. Each dependency's timeout starts when it becomes the active dependency.

### Fixed
- Stale-data race in dependency checking: a `file_contains` dependency listed after a `process_exited` dependency could be falsely satisfied by leftover data from a prior run, because both were previously checked concurrently.

## [0.8.1] - 2026-03-19

### Fixed
- JSONPath parse errors now include the expression that failed, making it easier to identify the problem in configs with multiple `file_contains` dependencies.

## [0.8.0] - 2026-03-19

### Breaking Changes
- `file_contains.key` now uses JSONPath (RFC 9535) instead of dot-separated paths. Existing keys like `database.url` must be rewritten as `"$.database.url"`. This also enables powerful new queries such as array filtering (e.g., `$.envs[?(@.alias == 'local')].rpc`).
- Undefined environment variables in dependency fields now produce an error. Previously, `$UNDEFINED` was silently kept as a literal string; procman now exits with an error message identifying the undefined variable.

### Added
- `-e` / `--env` repeatable CLI flag on `run`, `serve`, and `start` subcommands for injecting ad-hoc environment variables without modifying `procman.yaml`. Precedence: system env < `-e` flags < YAML `env:` block.
- Remaining-dependency logging: when a dependency is satisfied and others remain, the log line now lists the still-unsatisfied dependencies.
- JSONPath validation at parse time: invalid JSONPath expressions in `file_contains.key` are caught when the config is loaded rather than failing at runtime.

## [0.7.2] - 2026-03-19

### Added
- Multi-line `run` commands via YAML `|` block scalars, executed automatically via `sh -c`. Enables pipes, redirects, `&&`, and other shell features in multi-line scripts. Single-line commands continue to be tokenized and exec'd directly.
- Process group isolation: each child runs in its own process group (`setpgid`), ensuring shutdown signals reach all descendants — particularly important for `sh -c` subprocesses.
- Documentation: new mdbook chapters for CLI reference, dynamic process management, fan-out, logging, and shutdown. Existing chapters updated for multi-line `run` support.

### Changed
- Shutdown now signals process groups (`killpg`) instead of individual PIDs, ensuring shell subprocesses and other descendants are properly cleaned up.

### Fixed
- CI: dynamically fetch latest mdbook release version instead of hardcoding.
- CI: upgrade GitHub Actions to Node.js 24 compatible versions.

## [0.7.1] - 2026-03-19

### Added
- Environment variable expansion in dependency paths: `$VAR` and `${VAR}` references are expanded using the process environment (including per-process `env` overrides). Use `$$` for a literal `$`.
- Documentation site (mdbook) covering introduction, getting started, configuration, dependencies, and templates.

## [0.7.0] - 2026-03-19

### Breaking Changes
- CLI: removed FIFO path argument from `serve`, `start`, and `stop` subcommands. The FIFO path is now automatically derived from the config file path. `procman serve` and `procman stop` take an optional config path (default `procman.yaml`). `procman start` takes the command as a positional arg with an optional `--config` flag.

### Added
- `TcpConnect` dependency type: wait for a TCP port to become connectable (`tcp: "host:port"`, with optional `poll_interval` and `timeout_seconds`).
- `FileContainsKey` dependency type: wait for a file to contain a specific key (`file_contains` with `path`, `format` (json/yaml), `key` (dot-path), optional `env` for value extraction).
- Process output templates: processes receive `PROCMAN_OUTPUT` env var pointing to an output file. Other processes can reference values via `${{ process.KEY }}` in `run` and `env` fields.
- `for_each` glob fan-out: spawn one process instance per glob match (`for_each: {glob: "...", as: VAR}`). Fan-out group completion is tracked so `process_exited` dependencies on template names work transparently.
- Circular and unknown dependency detection at config parse time with descriptive error messages.
- Auto-derived FIFO paths from the canonical config file path.

### Changed
- Children are now signaled individually at shutdown (per-PID SIGTERM) instead of via process group.

### Fixed
- `ESRCH` error when a late-spawned process tried to join a process group whose leader had already exited.

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
