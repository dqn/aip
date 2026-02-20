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
- Active credentials file: `~/.codex/auth.json`
- Profiles: `~/.codex/profiles/<profile>/auth.json`
- Current profile marker: `~/.codex/profiles/_current`

## Notes

- `aip` (without subcommands) starts the interactive dashboard with profile switching, deletion, and live usage monitor.
- Switching profiles performs a safety sync check to avoid overwriting mismatched credentials.
- Usage display semantics differ by tool.
- Claude shows percentage as **used**.
- Codex shows percentage as **left**.

## Development

```bash
cargo test
```
