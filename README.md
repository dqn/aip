# aip

AI Profile Manager for Claude Code and Codex CLI.

## What It Does

- Manage multiple profiles for Claude Code and Codex CLI.
- Save current credentials into a named profile.
- Switch profiles interactively (`aip`) or non-interactively (`aip switch`).
- Show profile lists and current active profile.
- Run login flows and store credentials into a selected profile.
- Fetch and display usage windows for both tools.

## Requirements

- macOS (Claude profile handling uses Keychain via the `security` command).
- Rust toolchain that supports edition 2024.
- `claude` CLI in `PATH` for `aip login` (Claude).
- `codex` CLI in `PATH` for `aip login` (Codex).

## Installation

```bash
cargo install --path .
```

For local development:

```bash
cargo run -- <command>
```

## Commands

```bash
aip login [tool] [name] # run tool login and save to selected/new profile
aip save [tool] [name]  # save current credentials to a profile
aip list                # list all profiles
aip current             # show current profile per tool
aip                     # interactive mode (select tool + profile)
aip switch <tool> <name> # switch profile without prompts
aip usage               # show usage for current Claude and Codex profiles
aip delete [tool] [name] # delete a profile
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

- `aip` (without subcommands) starts interactive profile switching with usage preview.
- Switching profiles performs a safety sync check to avoid overwriting mismatched credentials.
- Usage display semantics differ by tool.
- Claude shows percentage as **used**.
- Codex shows percentage as **left**.

## Development

```bash
cargo test
```
