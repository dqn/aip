mod claude;
mod cli;
mod codex;
mod display;
mod tool;

use std::collections::HashMap;

use anyhow::Result;
use clap::Parser;
use console::{Key, Term};
use dialoguer::{Confirm, Input, Select};

use cli::{Cli, Command};
use display::{format_reset_time, format_usage_line, render_bar};
use tool::Tool;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        None => cmd_interactive().await?,
        Some(Command::Save) => cmd_save()?,
        Some(Command::Usage) => cmd_usage().await?,
        Some(Command::List) => cmd_list()?,
        Some(Command::Current) => cmd_current()?,
        Some(Command::Delete) => cmd_delete()?,
        Some(Command::Switch { tool, profile }) => {
            let tool: Tool = tool.parse()?;
            cmd_switch(tool, &profile)?;
        }
    }

    Ok(())
}

fn select_tool() -> Result<Tool> {
    let items = ["Claude Code", "Codex CLI"];
    let selection = Select::new()
        .with_prompt("Select tool")
        .items(&items)
        .default(0)
        .interact()?;

    Ok(Tool::ALL[selection])
}

fn select_profile(tool: Tool) -> Result<String> {
    let profiles = tool.list_profiles()?;
    if profiles.is_empty() {
        anyhow::bail!("no profiles found for {}", tool);
    }

    let current = tool.current_profile()?;
    let display_items: Vec<String> = profiles
        .iter()
        .map(|p| {
            if current.as_deref() == Some(p.as_str()) {
                format!("{} (current)", p)
            } else {
                p.clone()
            }
        })
        .collect();

    let selection = Select::new()
        .with_prompt("Select profile")
        .items(&display_items)
        .default(0)
        .interact()?;

    Ok(profiles[selection].clone())
}

async fn cmd_interactive() -> Result<()> {
    let tool = select_tool()?;

    let profiles = tool.list_profiles()?;
    if profiles.is_empty() {
        anyhow::bail!("no profiles found for {}", tool);
    }

    let current = tool.current_profile()?;

    // Prefetch usage for all profiles
    let usage_cache = prefetch_usage(tool, &profiles).await;

    // Select profile with usage preview
    let Some(selection) = select_profile_with_usage(&profiles, current.as_deref(), &usage_cache)?
    else {
        return Ok(());
    };
    let profile = &profiles[selection];

    if current.as_deref() == Some(profile.as_str()) {
        println!("Already on profile '{}'", profile);
        return Ok(());
    }

    cmd_switch(tool, profile)?;
    println!("Switched {} to profile '{}'", tool, profile);

    Ok(())
}

async fn prefetch_usage(tool: Tool, profiles: &[String]) -> HashMap<String, Vec<String>> {
    match tool {
        Tool::Claude => prefetch_claude_usage().await,
        Tool::Codex => prefetch_codex_usage(profiles),
    }
}

async fn prefetch_claude_usage() -> HashMap<String, Vec<String>> {
    let results = claude::usage::fetch_all_profiles_usage().await;
    results
        .into_iter()
        .map(|(profile, result)| {
            let lines = match result {
                Ok((usage, _info)) => {
                    vec![
                        format_usage_line(
                            "5-hour",
                            usage.five_hour.utilization,
                            usage.five_hour.resets_at,
                        ),
                        format_usage_line(
                            "Weekly",
                            usage.seven_day.utilization,
                            usage.seven_day.resets_at,
                        ),
                    ]
                }
                Err(e) => vec![format!("Error: {}", e)],
            };
            (profile, lines)
        })
        .collect()
}

fn prefetch_codex_usage(profiles: &[String]) -> HashMap<String, Vec<String>> {
    let lines = match codex::usage::fetch_usage() {
        Ok(Some(limits)) => {
            let mut lines = Vec::new();
            if let Some(primary) = &limits.primary {
                lines.push(format_usage_line(
                    "5-hour",
                    primary.used_percent,
                    primary.resets_at_utc(),
                ));
            }
            if let Some(secondary) = &limits.secondary {
                lines.push(format_usage_line(
                    "Weekly",
                    secondary.used_percent,
                    secondary.resets_at_utc(),
                ));
            }
            if lines.is_empty() {
                vec!["No usage data available".to_string()]
            } else {
                lines
            }
        }
        Ok(None) => vec!["No usage data available".to_string()],
        Err(e) => vec![format!("Error: {}", e)],
    };
    profiles
        .iter()
        .map(|p| (p.clone(), lines.clone()))
        .collect()
}

struct CursorGuard<'a>(&'a Term);

impl Drop for CursorGuard<'_> {
    fn drop(&mut self) {
        let _ = self.0.show_cursor();
    }
}

fn select_profile_with_usage(
    profiles: &[String],
    current: Option<&str>,
    usage_cache: &HashMap<String, Vec<String>>,
) -> Result<Option<usize>> {
    let term = Term::stderr();
    term.hide_cursor()?;
    let _guard = CursorGuard(&term);

    let default = profiles
        .iter()
        .position(|p| current == Some(p.as_str()))
        .unwrap_or(0);
    let mut selected = default;
    let mut rendered_lines: usize = 0;

    loop {
        if rendered_lines > 0 {
            term.clear_last_lines(rendered_lines)?;
        }

        let mut lines: usize = 0;

        term.write_line("Select profile:")?;
        lines += 1;

        for (i, profile) in profiles.iter().enumerate() {
            let cursor = if i == selected { ">" } else { " " };
            let suffix = if current == Some(profile.as_str()) {
                " (current)"
            } else {
                ""
            };
            term.write_line(&format!("{} {}{}", cursor, profile, suffix))?;
            lines += 1;
        }

        if let Some(usage_lines) = usage_cache.get(&profiles[selected])
            && !usage_lines.is_empty()
        {
            term.write_line("")?;
            lines += 1;
            for line in usage_lines {
                term.write_line(line)?;
                lines += 1;
            }
        }

        rendered_lines = lines;

        match term.read_key()? {
            Key::ArrowUp => {
                selected = selected.saturating_sub(1);
            }
            Key::ArrowDown => {
                if selected < profiles.len() - 1 {
                    selected += 1;
                }
            }
            Key::Enter => return Ok(Some(selected)),
            Key::Escape => return Ok(None),
            _ => {}
        }
    }
}

fn cmd_save() -> Result<()> {
    let tool = select_tool()?;

    let name: String = Input::new().with_prompt("Profile name").interact_text()?;

    match tool {
        Tool::Claude => claude::profile::save(&name)?,
        Tool::Codex => codex::profile::save(&name)?,
    }

    println!("Saved profile '{}' for {}", name, tool);
    Ok(())
}

async fn cmd_usage() -> Result<()> {
    for tool in Tool::ALL {
        print_usage_for_tool(tool).await;
    }
    Ok(())
}

async fn print_usage_for_tool(tool: Tool) {
    let current = tool.current_profile().ok().flatten();
    let label = match &current {
        Some(name) => format!("{} [{}]", tool, name),
        None => format!("{}", tool),
    };

    match tool {
        Tool::Claude => print_claude_usage(&label).await,
        Tool::Codex => print_codex_usage(&label),
    }
}

async fn print_claude_usage(label: &str) {
    match claude::usage::fetch_usage().await {
        Ok((usage, info)) => {
            let suffix = match info.plan_type.as_deref() {
                Some(plan) => format!(" ({})", plan),
                None => String::new(),
            };
            println!("{}{}", label, suffix);
            println!(
                "  5-hour  {}  {:>5.1}%  resets at {}",
                render_bar(usage.five_hour.utilization),
                usage.five_hour.utilization,
                format_reset_time(usage.five_hour.resets_at),
            );
            println!(
                "  Weekly  {}  {:>5.1}%  resets at {}",
                render_bar(usage.seven_day.utilization),
                usage.seven_day.utilization,
                format_reset_time(usage.seven_day.resets_at),
            );
        }
        Err(e) => {
            println!("{}", label);
            println!("  Error: {}", e);
        }
    }
    println!();
}

fn print_codex_usage(label: &str) {
    match codex::usage::fetch_usage() {
        Ok(Some(limits)) => {
            println!("{}", label);
            if let Some(primary) = &limits.primary {
                println!(
                    "  5-hour  {}  {:>5.1}%  resets at {}",
                    render_bar(primary.used_percent),
                    primary.used_percent,
                    format_reset_time(primary.resets_at_utc()),
                );
            }
            if let Some(secondary) = &limits.secondary {
                println!(
                    "  Weekly  {}  {:>5.1}%  resets at {}",
                    render_bar(secondary.used_percent),
                    secondary.used_percent,
                    format_reset_time(secondary.resets_at_utc()),
                );
            }
        }
        Ok(None) => {
            println!("{}", label);
            println!("  No usage data available");
        }
        Err(e) => {
            println!("{}", label);
            println!("  Error: {}", e);
        }
    }
    println!();
}

fn cmd_list() -> Result<()> {
    for tool in Tool::ALL {
        let profiles = tool.list_profiles()?;
        let current = tool.current_profile()?;

        println!("{}:", tool);
        if profiles.is_empty() {
            println!("  (no profiles)");
        } else {
            for p in &profiles {
                let marker = if current.as_deref() == Some(p.as_str()) {
                    " *"
                } else {
                    ""
                };
                println!("  {}{}", p, marker);
            }
        }
        println!();
    }
    Ok(())
}

fn cmd_current() -> Result<()> {
    for tool in Tool::ALL {
        let current = tool.current_profile()?;
        match current {
            Some(name) => println!("{}: {}", tool, name),
            None => println!("{}: (none)", tool),
        }
    }
    Ok(())
}

fn cmd_switch(tool: Tool, profile: &str) -> Result<()> {
    match tool {
        Tool::Claude => claude::profile::switch(profile)?,
        Tool::Codex => codex::profile::switch(profile)?,
    }
    Ok(())
}

fn cmd_delete() -> Result<()> {
    let tool = select_tool()?;
    let profile = select_profile(tool)?;

    if !Confirm::new()
        .with_prompt(format!("Delete profile '{}' for {}?", profile, tool))
        .default(false)
        .interact()?
    {
        println!("Cancelled");
        return Ok(());
    }

    match tool {
        Tool::Claude => claude::profile::delete(&profile)?,
        Tool::Codex => codex::profile::delete(&profile)?,
    }

    println!("Deleted profile '{}' for {}", profile, tool);
    Ok(())
}
