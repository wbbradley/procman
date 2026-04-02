# Changelog

## [0.22.0] - 2026-04-01

### Added
- **`task` process type:** New `task` keyword for defining run-to-completion processes triggered via the `-t` / `--task` CLI flag. Tasks do not autostart and their failures do not tear down the supervisor — procman waits for all triggered tasks to complete and exits with the first non-zero task exit code (or 0 if all succeed). Designed for test harness orchestration.
- **`-t` / `--task` CLI flag:** Repeatable flag to activate specific tasks by name (e.g., `procman tests.pman -t test_a -t test_b`).
- **`${args.NAME}` in import paths:** Import paths can now reference root-level arg values (e.g., `import "${args.dep_dir}/db.pman" as db`). The compilation pipeline has been restructured to resolve root-level args before loading imports.

### Changed
- **`--check` mode is more lenient with parameterized imports:** When running `--check` without providing all required root args, parameterized imports (those containing `${args.NAME}`) are skipped with a warning instead of producing a hard error. Literal import path failures remain errors.

## [0.21.0] - 2026-03-30

### Breaking Changes
- `arg` declarations must now appear at the top level, outside the `config { }` block. Placing `arg` inside `config` is now a parse error.
- `env` declarations must now appear at the top level, outside the `config { }` block. Placing `env` inside `config` is now a parse error.
- The `config { }` block now only accepts `logs` and `log_time` settings.

### Added
- **Module imports:** `.pman` files can import other `.pman` files using `import "path.pman" as alias`. Imported entities are namespaced under the alias (e.g., `db::migrate`) in logs, dependency references, and runtime process names.
- **Parameterized imports:** Import statements accept bindings to configure the imported module's args: `import "db.pman" as db { url = "postgres://..." }`. Binding expressions are evaluated in the importing file's context.
- **Nested imports:** Imported files may themselves contain `import` statements. Each module's imports are private; transitive namespaces are not accessible from parent modules.
- **Namespaced cross-module references:** `@alias::name` syntax for referencing imported entities in `after`, `on_fail spawn`, and `@job.KEY` output reference positions.
- **Namespaced args refs:** `alias::args.name` syntax for referencing an imported module's resolved arg values in any expression position.
- **CLI flags for imported module args:** Unbound imported args (no binding and no default) are automatically exposed as `--alias::arg-name` CLI flags. Bound or defaulted args can be overridden via the same syntax.
- **Single-line env shorthand:** `env KEY = expr` is now accepted as a top-level statement alongside the block form `env { ... }`. Both forms can coexist.
- Cross-module validation: import bindings, namespaced after/output/spawn refs, and namespaced args refs are all validated at parse time.
- Diamond import detection: two imports resolving to the same canonical file within one module produce an error.
- Import cycle detection across the full transitive import graph.

### Changed
- `-- --help` output now groups arguments by namespace, with imported module args shown in labeled sections.

### Fixed
- Test-only functions gated behind `#[cfg(test)]` to eliminate dead_code warnings.

## [0.20.0] - 2026-03-29

### Breaking Changes
- Fenced string syntax changed from triple-backtick to triple-quote (`"""`). All multi-line `run` blocks in `.pman` files must now use `"""` as the delimiter.

## [0.19.0] - 2026-03-29

### Added
- `poll` and `timeout` options are now supported on all wait conditions (`exists`, `!exists`, `!running`, `after`). Previously only `http`, `connect`, `!connect`, and `contains` accepted these options. Defaults are unchanged (poll = 1s, or 100ms for `after`; timeout = none).
- Language Design spec added to the [mdbook documentation](https://wbbradley.github.io/procman/language-design.html).

### Changed
- Repositioned `.pman` from "DSL" to "language" across all documentation and metadata surfaces (README, Cargo.toml, CLI help, docs, design spec).
- Corrected documentation to reflect actual default `timeout` for wait conditions: `none` (wait indefinitely), not `60s`. Runtime behavior is unchanged.
- Documented both `"json"` and `"yaml"` as supported `contains` condition formats.
- Expanded parse-time validation rules in the language design spec (duplicate names, namespace collisions, duplicate watch names).

### Removed
- Reserved keyword `as` removed from the lexer. It was never part of the grammar; removing it frees the identifier for use as a job/service/event name.

## [0.18.0] - 2026-03-28

### Breaking Changes
- **Removed YAML config file support.** procman no longer accepts `.yaml` or `.yml` configuration files. Only the `.pman` format is now supported. Users with YAML config files must convert them to the `.pman` format.

### Added
- `log_time` config option: when `log_time = true` is set in the `config` block, every log line is prefixed with elapsed time since procman started (e.g., `api 1.2s | listening on :3000`). Defaults to `false`.

## [0.17.0] - 2026-03-28

### Breaking Changes
- **`once` keyword removed:** The `once = true`/`once = false` property in job bodies is no longer valid syntax. Jobs are now inherently one-shot (run to completion). Remove all `once` lines from config files.
- **New `service` keyword for long-running processes:** The `job` keyword now means "run to completion" (one-shot). Long-running daemons must use the new `service` keyword. Both `service name { }` and `service name if expr { }` are supported with the same body fields as `job`.
- **Default log directory changed** from `procman-logs` to `logs/procman`.
- Dependency timeout defaults changed: wait conditions now default to no timeout (infinite wait) instead of 60 seconds when no explicit `timeout` is specified. Use `timeout = 60s` to restore the old behavior.

### Added
- `--check` CLI flag: validates the config file (parsing, arg definitions, template resolution, dependency cycle detection, all static checks) and exits with `<path>: ok` on success, without starting any processes. Useful for CI linting and editor integration.
- `service` keyword in `.pman` for declaring long-running daemon processes, with full support for `if` conditions, `env`, `wait`, `watch`, and `for` blocks.

### Changed
- Error messages from the `.pman` parser and lexer now use a standardized `<file>:<line>:<col>: error: <description>` format, matching common compiler diagnostic conventions for editor gutter integration.
- Validation messages updated to reflect the new `job`/`service` distinction.

### Fixed
- A process waiting on `after @job` where the job exits with a non-zero code now correctly triggers shutdown instead of hanging indefinitely.

## [0.16.0] - 2026-03-24

### Fixed
- Glob for-loop `env` bindings (e.g., `env NODE_CONFIG = config_path` inside a `for ... in glob(...)` block) were silently dropped during lowering, leaving the variables unbound at runtime. Array and range iterables were unaffected.

## [0.15.2] - 2026-03-24

### Added
- New `.pman` language as an alternative to YAML. When the config file path ends with `.pman`, procman uses a purpose-built parser supporting `config {}`, `job {}`, `event {}` blocks with `env`, `wait`, `watch`, `for` loops, conditional jobs (`job name if expr {}`), and an expression language. The YAML format continues to work unchanged.
- Source-location context (file, line, column) in `.pman` parser error messages.

### Fixed
- Stray child PIDs reaped by `waitpid` no longer influence the supervisor's exit code.
- Tests no longer flake due to mutex poisoning from prior test panics.
- A race condition in a dependency test was fixed by joining the spawned thread before asserting.

## [0.15.1] - 2026-03-24

### Fixed
- `for_each` glob patterns containing environment variables (e.g., `$DIR/node-*.yaml`) are now expanded before glob matching. Previously, the raw unexpanded string was passed to the glob engine, causing zero matches.
- `${{ args.* }}` templates now work in `for_each` glob patterns, dependency fields (`url`, `tcp`, `path`, `not_listening`, `not_exists`, `not_running`), and watch check fields. Previously, only `run`, `env`, and `condition` supported arg templates.
- Environment variable expansion in dependencies and watches now runs after arg template resolution, fixing cases where `${{ args.* }}` placeholders in these fields were misinterpreted.
- Invalid characters in braced env var references (e.g., `${VAR:-fallback}`) now produce a clear parse-time error instead of a confusing "undefined variable" error at runtime.
- `for_each` glob patterns with invalid env var references are now caught at config parse time rather than at process spawn time.

### Changed
- Documentation (mdbook) updated to reflect current CLI, `jobs:`/`config:` YAML structure, `config.args`, `condition:` field, expanded `process_exited` form, `watch`, and `autostart`. Removed stale references to removed `serve`/`start`/`stop` subcommands.

## [0.15.0] - 2026-03-23

### Added
- Conditional process execution via `condition:` field. A shell command is evaluated before spawning; if it exits non-zero, the job is skipped. Skipped `once: true` jobs are registered as exited so `process_exited` dependents can proceed.
- Infinite wait on `process_exited` dependencies via expanded object form: `process_exited: { name: ..., timeout_seconds: null }`. The simple string form retains the 60-second default.

## [0.14.0] - 2026-03-23

### Added
- User-defined CLI arguments via `config.args`. Define typed parameters (string/bool) in the config file, parsed from argv after `--`. Args can inject env vars via `env:` field and are available as `${{ args.name }}` templates in `run` and `env` fields.
- `-- --help` prints usage for config-defined args.
- Args without a `default` are required; args with a default are optional.

## [0.13.0] - 2026-03-23

### Breaking Changes
- Removed `serve`, `start`, and `stop` subcommands and the entire FIFO system. The CLI is now `procman <file> [-e KEY=VALUE]... [--debug]`.
- New YAML config format: process definitions must be placed under a `jobs:` key. An optional `config:` section supports global settings. Old flat-format files produce a clear migration error.
- Config file path is now a required positional argument (no default).

### Added
- `config.logs` option to customize the log directory path (default: `logs/procman`).

### Removed
- `fifo.rs`, `fifo_path.rs`, FIFO-based IPC, advisory flock, `shell-words` dependency.
- `serve_mode` flag from `ProcessGroup`.

## [0.12.0] - 2026-03-23

### Added
- Runtime health watches: processes can define `watch` entries that continuously poll health checks (HTTP, TCP, file existence, etc.) after startup. Configurable `initial_delay`, `poll_interval`, and `failure_threshold`.
- Watch failure actions: `shutdown` (default), `debug` (pause for inspection), `log` (log-only), or `spawn: <process>` (start a dormant process with `PROCMAN_WATCH_*` context env vars).
- `autostart: false` for dormant processes that are not started until explicitly spawned by a watch action or `procman start` command.
- Duplicate-spawn protection: spawning an already-running process is silently skipped with a log message.
- Watches on `for_each` processes are automatically cloned per fan-out instance with check targets substituted.

### Changed
- Internal: dependency check functions extracted into shared `checks.rs` module for reuse by both dependency waiting and watch polling.

## [0.11.0] - 2026-03-22

### Breaking Changes
- All `run` commands now execute under `sh -e -u -o pipefail -c`, enabling strict shell error handling. Commands that silently swallowed mid-script errors, referenced unset variables, or ignored pipeline failures will now fail. To opt out per-command, prefix with `set +e`, `set +u`, or `set +o pipefail` as needed.

### Changed
- README: added "Dependency graph" section foregrounding the declarative DAG model with a concrete YAML example; reframed "Scripted service bringup" as an escape hatch.

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
- Combined log file: all formatted log output (the `"name | line"` view shown on stdout) is now also written to `logs/procman/procman.log`, providing a single file with interleaved output from all processes.

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
