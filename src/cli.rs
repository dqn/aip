use std::ffi::{OsStr, OsString};

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    about = "AI Profile Manager - manage profiles for Claude Code and Codex CLI",
    version
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

pub fn normalize_short_version_flag<I, T>(args: I) -> Vec<OsString>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let mut normalized: Vec<OsString> = args.into_iter().map(Into::into).collect();
    if let Some(arg) = normalized.get(1) {
        if arg.as_os_str() == OsStr::new("-v") {
            normalized[1] = OsString::from("--version");
        } else if arg.as_os_str() == OsStr::new("-h") {
            normalized[1] = OsString::from("--help");
        }
    }
    normalized
}

#[derive(Subcommand)]
pub enum Command {
    /// Save current credentials as a new profile
    Save {
        /// Tool name (claude or codex)
        tool: Option<String>,
        /// Profile name
        profile: Option<String>,
    },
    /// Delete a profile
    Delete {
        /// Tool name (claude or codex)
        tool: Option<String>,
        /// Profile name
        profile: Option<String>,
    },
    /// Switch profile (non-interactive)
    Switch {
        /// Tool name (claude or codex)
        tool: String,
        /// Profile name
        profile: String,
    },
    /// Log in and save credentials to a profile
    Login {
        /// Tool name (claude or codex)
        tool: Option<String>,
        /// Profile name
        profile: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_command_accepts_tool_and_name_as_positional_args() {
        let parsed = Cli::try_parse_from(["aip", "login", "claude", "work"]);

        assert!(parsed.is_ok());
        assert!(matches!(
            parsed.unwrap().command,
            Some(Command::Login {
                tool: Some(tool),
                profile: Some(profile),
            }) if tool == "claude" && profile == "work"
        ));
    }

    #[test]
    fn login_command_accepts_only_tool_as_positional_arg() {
        let parsed = Cli::try_parse_from(["aip", "login", "codex"]);

        assert!(parsed.is_ok());
        assert!(matches!(
            parsed.unwrap().command,
            Some(Command::Login {
                tool: Some(tool),
                profile: None,
            }) if tool == "codex"
        ));
    }

    #[test]
    fn login_command_without_args_is_still_supported() {
        let parsed = Cli::try_parse_from(["aip", "login"]);

        assert!(parsed.is_ok());
        assert!(matches!(
            parsed.unwrap().command,
            Some(Command::Login {
                tool: None,
                profile: None,
            })
        ));
    }

    #[test]
    fn version_long_option_displays_version() {
        let parsed = Cli::try_parse_from(["aip", "--version"]);
        assert!(parsed.is_err());
        let error = parsed
            .err()
            .expect("expected --version to trigger clap output");

        assert_eq!(error.kind(), clap::error::ErrorKind::DisplayVersion);
    }

    #[test]
    fn version_short_option_displays_version() {
        let parsed = Cli::try_parse_from(normalize_short_version_flag(["aip", "-v"]));
        assert!(parsed.is_err());
        let error = parsed.err().expect("expected -v to trigger clap output");

        assert_eq!(error.kind(), clap::error::ErrorKind::DisplayVersion);
    }

    #[test]
    fn help_long_option_displays_help() {
        let parsed = Cli::try_parse_from(["aip", "--help"]);
        assert!(parsed.is_err());
        let error = parsed
            .err()
            .expect("expected --help to trigger clap output");

        assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
    }

    #[test]
    fn help_short_option_displays_help() {
        let parsed = Cli::try_parse_from(normalize_short_version_flag(["aip", "-h"]));
        assert!(parsed.is_err());
        let error = parsed.err().expect("expected -h to trigger clap output");

        assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
    }

    #[test]
    fn normalize_short_version_flag_converts_short_help_to_long_help() {
        let normalized = normalize_short_version_flag(["aip", "-h"]);

        assert_eq!(
            normalized,
            vec![OsString::from("aip"), OsString::from("--help"),]
        );
    }

    #[test]
    fn normalize_short_version_flag_only_changes_first_cli_arg() {
        let normalized = normalize_short_version_flag(["aip", "login", "-v"]);

        assert_eq!(
            normalized,
            vec![
                OsString::from("aip"),
                OsString::from("login"),
                OsString::from("-v"),
            ]
        );
    }
}
