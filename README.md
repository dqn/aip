# aip

AI Profile Manager for Claude Code and Codex CLI.

## What It Does

- Manage multiple profiles for Claude Code and Codex CLI.
- Save current credentials into a named profile.
- Switch profiles and delete profiles interactively from the dashboard.
- Fetch and display usage windows for both tools with auto-refresh.

## Requirements

- macOS (Claude profile handling uses Keychain via the `security` command).
- Rust toolchain that supports edition 2024.

## Installation

```bash
cargo install aip-cli
```

For local development:

```bash
cargo run -- <command>
```

## Commands

```bash
aip                    # interactive dashboard (switch, delete, usage monitor)
aip save [tool] [name] # save current credentials to a profile
aip codex [args...]    # launch Codex with the selected account
aip -h, aip --help     # show command help
aip -v, aip --version  # show aip version
```

`tool` values: `claude` or `codex`

## Profile Storage

### Claude Code

- Base directory: `~/.claude`
- Profiles: `~/.claude/profiles/<profile>/credentials.json`
- Current profile marker: `~/.claude/profiles/_current`
- Active credentials source: macOS Keychain service `Claude Code-credentials`

### Codex CLI

- Base directory: `~/.codex`
- Imported credentials: `CODEX_HOME/auth.json` when `CODEX_HOME` is set, otherwise `~/.codex/auth.json`.
- Each profile is a separate Codex home: `~/.codex/profiles/<profile>`.
- Credentials: `~/.codex/profiles/<profile>/auth.json`, updated directly by Codex.
- Selected profile marker: `~/.codex/profiles/_current`; it controls the next `aip codex` launch.

Select an account in the dashboard, then run `aip codex` instead of `codex`. Native arguments are forwarded, for example `aip codex resume --last` or `aip codex --help`. This launcher uses the profile's `CODEX_HOME`, file-based authentication, and separate SQLite state. It inherits the current directory, terminal, signals, and Codex's exit status.

For each imported ChatGPT OAuth profile, first run `aip codex login` and sign in to that account once. This creates an independent OAuth grant in its home, so token refresh does not compete with the original session. The launcher refuses to start using an imported refresh token until it changes through login. This also applies to profiles saved by older aip versions. API-key profiles do not need this step.

Existing global configuration, instructions, skills, rules, hooks, plugins, and named `*.config.toml` configuration profiles are linked into a profile home on launch. Existing profile-specific settings are preserved. Credentials, server sockets, and session history are not shared. Use `aip codex resume` for that account's launcher sessions; previously created global sessions remain available through plain `codex resume`.

## Notes

- `aip` (without subcommands) starts the interactive dashboard with profile switching, deletion, and live usage monitor.
- Switching Claude profiles performs a safety sync check to avoid overwriting mismatched credentials.
- Selecting a Codex profile only changes the next launch. Existing sessions keep their original account and server; aip does not restart servers or replace their credentials. Plain `codex` and the desktop app do not follow aip's selection.
- A Codex profile with a server control socket cannot be deleted or have credentials imported over it. Stop that profile's server before removing or replacing it. Saving a profile's own current credentials does not rewrite them.
- Codex handles token refresh. Usage monitoring reads profile credentials without rotating tokens; if a dormant profile's token has expired, launch that profile to renew it.
- Usage display semantics differ by tool.
- Claude shows percentage as **used**.
- Codex shows percentage as **left**.

## Development

```bash
cargo test
```
