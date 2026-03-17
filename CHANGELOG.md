# Changelog

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
