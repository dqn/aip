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

pub fn normalize_short_flags<I, T>(args: I) -> Vec<OsString>
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
    /// Launch Codex using the selected account without interrupting other sessions
    #[command(disable_help_flag = true, disable_version_flag = true)]
    Codex {
        /// Arguments passed directly to Codex
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
    /// Save current credentials as a new profile
    Save {
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
    fn codex_launch_accepts_native_arguments() {
        let cli = Cli::try_parse_from(["aip", "codex", "resume", "--last", "--model", "example"])
            .unwrap();
        assert!(
            matches!(cli.command, Some(Command::Codex { args }) if args == ["resume", "--last", "--model", "example"])
        );
    }

    #[test]
    fn codex_help_version_and_separator_are_forwarded() {
        for flag in ["--help", "--version", "-h", "-v"] {
            let cli = Cli::try_parse_from(normalize_short_flags(["aip", "codex", flag])).unwrap();
            assert!(matches!(cli.command, Some(Command::Codex { args }) if args == [flag]));
        }
        let cli =
            Cli::try_parse_from(["aip", "codex", "--", "exec", "a prompt with spaces"]).unwrap();
        assert!(
            matches!(cli.command, Some(Command::Codex { args }) if args == ["exec", "a prompt with spaces"])
        );
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
        let parsed = Cli::try_parse_from(normalize_short_flags(["aip", "-v"]));
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
        let parsed = Cli::try_parse_from(normalize_short_flags(["aip", "-h"]));
        assert!(parsed.is_err());
        let error = parsed.err().expect("expected -h to trigger clap output");

        assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
    }

    #[test]
    fn normalize_short_flags_converts_short_help_to_long_help() {
        let normalized = normalize_short_flags(["aip", "-h"]);

        assert_eq!(
            normalized,
            vec![OsString::from("aip"), OsString::from("--help"),]
        );
    }

    #[test]
    fn normalize_short_flags_only_changes_first_cli_arg() {
        let normalized = normalize_short_flags(["aip", "save", "-v"]);

        assert_eq!(
            normalized,
            vec![
                OsString::from("aip"),
                OsString::from("save"),
                OsString::from("-v"),
            ]
        );
    }
}
