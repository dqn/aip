mod claude;
mod cli;
mod codex;
mod display;
mod tool;

use std::collections::HashMap;
use std::fs;
use std::process::Command as ProcessCommand;
use std::time::Duration;

use anyhow::{Result, anyhow};
use chrono::Local;
use clap::Parser;
use console::{Key, Term};
use dialoguer::{Confirm, Input, Select};

use cli::{Cli, Command};
use codex::usage::RateLimits;
use display::{DisplayMode, format_usage_line};
use tool::Tool;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse_from(cli::normalize_short_version_flag(std::env::args_os()));

    match cli.command {
        None => cmd_interactive().await?,
        Some(Command::Save { tool, profile }) => cmd_save(tool, profile)?,
        Some(Command::Usage) => cmd_usage().await?,
        Some(Command::List) => cmd_list()?,
        Some(Command::Current) => cmd_current()?,
        Some(Command::Delete { tool, profile }) => cmd_delete(tool, profile)?,
        Some(Command::Switch { tool, profile }) => {
            let tool: Tool = tool.parse()?;
            cmd_switch(tool, &profile)?;
        }
        Some(Command::Login { tool, profile }) => cmd_login(tool, profile)?,
    }

    Ok(())
}

const STARTUP_PREFETCH_TIMEOUT: Duration = Duration::from_millis(250);
const PREFETCH_POLL_INTERVAL: Duration = Duration::from_millis(100);
const PREFETCH_RECHECK_TIMEOUT: Duration = Duration::from_millis(1);
const USAGE_MONITOR_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

struct StartupUsagePrefetch {
    claude: PrefetchState,
    codex: PrefetchState,
}

enum PrefetchState {
    Pending(tokio::task::JoinHandle<HashMap<String, Vec<String>>>),
    Ready(HashMap<String, Vec<String>>),
    Failed,
}

impl StartupUsagePrefetch {
    fn start() -> Self {
        let codex_profiles = Tool::Codex.list_profiles().unwrap_or_default();
        Self {
            claude: PrefetchState::Pending(spawn_prefetch_task(Tool::Claude, Vec::new())),
            codex: PrefetchState::Pending(spawn_prefetch_task(Tool::Codex, codex_profiles)),
        }
    }

    async fn usage_cache(&mut self, tool: Tool) -> HashMap<String, Vec<String>> {
        self.state_mut(tool)
            .usage_cache_with_timeout(STARTUP_PREFETCH_TIMEOUT)
            .await
    }

    fn state_mut(&mut self, tool: Tool) -> &mut PrefetchState {
        match tool {
            Tool::Claude => &mut self.claude,
            Tool::Codex => &mut self.codex,
        }
    }
}

impl PrefetchState {
    async fn usage_cache_with_timeout(
        &mut self,
        timeout: Duration,
    ) -> HashMap<String, Vec<String>> {
        match self {
            PrefetchState::Ready(cache) => cache.clone(),
            PrefetchState::Failed => HashMap::new(),
            PrefetchState::Pending(handle) => match tokio::time::timeout(timeout, handle).await {
                Ok(Ok(cache)) => {
                    *self = PrefetchState::Ready(cache.clone());
                    cache
                }
                Ok(Err(_)) => {
                    *self = PrefetchState::Failed;
                    HashMap::new()
                }
                Err(_) => HashMap::new(),
            },
        }
    }

    fn is_pending(&self) -> bool {
        matches!(self, PrefetchState::Pending(_))
    }
}

fn spawn_prefetch_task(
    tool: Tool,
    profiles: Vec<String>,
) -> tokio::task::JoinHandle<HashMap<String, Vec<String>>> {
    tokio::spawn(async move { prefetch_usage(tool, &profiles).await })
}

fn select_tool() -> Result<Option<Tool>> {
    let items = ["Claude Code", "Codex CLI"];
    let selection = Select::new()
        .with_prompt("Select tool")
        .items(&items)
        .default(0)
        .interact_opt()?;

    Ok(selection.map(|i| Tool::ALL[i]))
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
    let mut startup_prefetch = StartupUsagePrefetch::start();

    loop {
        let Some(tool) = select_tool()? else {
            return Ok(());
        };

        let profiles = tool.list_profiles()?;
        if profiles.is_empty() {
            anyhow::bail!("no profiles found for {}", tool);
        }

        let current = tool.current_profile()?;

        let mut usage_cache = startup_prefetch.usage_cache(tool).await;
        let prefetch_state = startup_prefetch.state_mut(tool);

        // Select profile with usage preview
        let Some(selection) = select_profile_with_usage(
            &profiles,
            current.as_deref(),
            &mut usage_cache,
            prefetch_state,
        )
        .await?
        else {
            continue;
        };
        let profile = &profiles[selection];

        if current.as_deref() == Some(profile.as_str()) {
            println!("Already on profile '{}'", profile);
            return Ok(());
        }

        cmd_switch(tool, profile)?;
        println!("Switched {} to profile '{}'", tool, profile);

        return Ok(());
    }
}

async fn prefetch_usage(tool: Tool, profiles: &[String]) -> HashMap<String, Vec<String>> {
    match tool {
        Tool::Claude => prefetch_claude_usage().await,
        Tool::Codex => prefetch_codex_usage(profiles).await,
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
                            &DisplayMode::Used,
                        ),
                        format_usage_line(
                            "Weekly",
                            usage.seven_day.utilization,
                            usage.seven_day.resets_at,
                            &DisplayMode::Used,
                        ),
                    ]
                }
                Err(e) => vec![format!("Error: {}", e)],
            };
            (profile, lines)
        })
        .collect()
}

fn codex_usage_lines(result: Result<Option<RateLimits>>) -> Vec<String> {
    match result {
        Ok(Some(limits)) => {
            let mut lines = Vec::new();
            if let Some(primary) = &limits.primary {
                lines.push(format_usage_line(
                    "5-hour",
                    primary.used_percent,
                    Some(primary.resets_at_utc()),
                    &DisplayMode::Left,
                ));
            }
            if let Some(secondary) = &limits.secondary {
                lines.push(format_usage_line(
                    "Weekly",
                    secondary.used_percent,
                    Some(secondary.resets_at_utc()),
                    &DisplayMode::Left,
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
    }
}

async fn prefetch_codex_usage(profiles: &[String]) -> HashMap<String, Vec<String>> {
    let current = Tool::Codex.current_profile().ok().flatten();

    let mut handles = Vec::new();
    for p in profiles {
        let p = p.clone();
        let is_current = current.as_deref() == Some(p.as_str());
        handles.push(tokio::spawn(async move {
            let result = if is_current {
                codex::usage::fetch_usage().await
            } else {
                match Tool::Codex.profile_dir(&p) {
                    Ok(dir) => {
                        let auth_path = dir.join("auth.json");
                        codex::usage::fetch_usage_from_auth(&auth_path).await
                    }
                    Err(e) => Err(e),
                }
            };
            (p, codex_usage_lines(result))
        }));
    }

    let mut results = HashMap::new();
    for handle in handles {
        if let Ok((p, lines)) = handle.await {
            results.insert(p, lines);
        }
    }
    results
}

struct CursorGuard<'a>(&'a Term);

impl Drop for CursorGuard<'_> {
    fn drop(&mut self) {
        let _ = self.0.show_cursor();
    }
}

fn spawn_read_key_task() -> tokio::task::JoinHandle<std::io::Result<Key>> {
    tokio::task::spawn_blocking(|| Term::stderr().read_key())
}

async fn refresh_usage_cache(
    usage_cache: &mut HashMap<String, Vec<String>>,
    prefetch_state: &mut PrefetchState,
) -> bool {
    let was_pending = prefetch_state.is_pending();
    let updated = prefetch_state
        .usage_cache_with_timeout(PREFETCH_RECHECK_TIMEOUT)
        .await;
    let is_pending = prefetch_state.is_pending();

    let mut changed = false;
    if !updated.is_empty() && *usage_cache != updated {
        *usage_cache = updated;
        changed = true;
    }
    if was_pending != is_pending {
        changed = true;
    }

    changed
}

async fn select_profile_with_usage(
    profiles: &[String],
    current: Option<&str>,
    usage_cache: &mut HashMap<String, Vec<String>>,
    prefetch_state: &mut PrefetchState,
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
    let mut needs_render = true;
    let mut key_task = spawn_read_key_task();

    loop {
        if needs_render {
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
            } else if prefetch_state.is_pending() {
                term.write_line("")?;
                term.write_line("Loading usage...")?;
                lines += 2;
            }

            rendered_lines = lines;
            needs_render = false;
        }

        if prefetch_state.is_pending() {
            tokio::select! {
                key_result = &mut key_task => {
                    let key = key_result
                        .map_err(|e| anyhow!("failed to read key input: {}", e))??;
                    match key {
                        Key::ArrowUp => {
                            selected = selected.saturating_sub(1);
                            needs_render = true;
                        }
                        Key::ArrowDown => {
                            if selected < profiles.len() - 1 {
                                selected += 1;
                                needs_render = true;
                            }
                        }
                        Key::Enter => return Ok(Some(selected)),
                        Key::Escape => {
                            term.clear_last_lines(rendered_lines)?;
                            return Ok(None);
                        }
                        _ => {}
                    }
                    key_task = spawn_read_key_task();
                }
                _ = tokio::time::sleep(PREFETCH_POLL_INTERVAL) => {
                    if refresh_usage_cache(usage_cache, prefetch_state).await {
                        needs_render = true;
                    }
                }
            }
        } else {
            let key = (&mut key_task)
                .await
                .map_err(|e| anyhow!("failed to read key input: {}", e))??;
            match key {
                Key::ArrowUp => {
                    selected = selected.saturating_sub(1);
                    needs_render = true;
                }
                Key::ArrowDown => {
                    if selected < profiles.len() - 1 {
                        selected += 1;
                        needs_render = true;
                    }
                }
                Key::Enter => return Ok(Some(selected)),
                Key::Escape => {
                    term.clear_last_lines(rendered_lines)?;
                    return Ok(None);
                }
                _ => {}
            }
            key_task = spawn_read_key_task();
        }
    }
}

fn build_usage_monitor_lines(
    tool_profiles: &[(Tool, Vec<String>, Option<String>)],
    usage_caches: &HashMap<Tool, HashMap<String, Vec<String>>>,
) -> Vec<String> {
    let timestamp = Local::now().format("%H:%M:%S");
    let mut lines = Vec::new();
    lines.push(format!(
        "Usage Monitor (5s refresh, ESC/q to quit)  Updated: {}",
        timestamp
    ));
    lines.push(String::new());

    for (tool, profiles, current) in tool_profiles {
        lines.push(format!("{}", tool));

        if profiles.is_empty() {
            lines.push("  (no profiles)".to_string());
        } else {
            let cache = usage_caches.get(tool);
            for profile in profiles {
                let marker = if current.as_deref() == Some(profile.as_str()) {
                    " *"
                } else {
                    ""
                };
                lines.push(format!("  {}{}", profile, marker));

                if let Some(usage_lines) = cache.and_then(|c| c.get(profile)) {
                    for line in usage_lines {
                        lines.push(format!("    {}", line));
                    }
                } else {
                    lines.push("    (no data)".to_string());
                }
            }
        }

        lines.push(String::new());
    }

    lines
}

fn cmd_save(tool_arg: Option<String>, profile_arg: Option<String>) -> Result<()> {
    let tool = match tool_arg {
        Some(t) => t.parse()?,
        None => {
            let Some(t) = select_tool()? else {
                return Ok(());
            };
            t
        }
    };

    let name = match profile_arg {
        Some(p) => p,
        None => Input::new().with_prompt("Profile name").interact_text()?,
    };

    match tool {
        Tool::Claude => claude::profile::save(&name)?,
        Tool::Codex => codex::profile::save(&name)?,
    }

    println!("Saved profile '{}' for {}", name, tool);
    Ok(())
}

async fn cmd_usage() -> Result<()> {
    let term = Term::stderr();
    term.hide_cursor()?;
    let _guard = CursorGuard(&term);

    term.write_line("Fetching usage...")?;
    let mut rendered_lines: usize = 1;

    let mut key_task = spawn_read_key_task();

    loop {
        let tool_profiles: Vec<(Tool, Vec<String>, Option<String>)> = Tool::ALL
            .iter()
            .map(|&t| {
                let profiles = t.list_profiles().unwrap_or_default();
                let current = t.current_profile().ok().flatten();
                (t, profiles, current)
            })
            .collect();

        let codex_profiles: Vec<String> = tool_profiles
            .iter()
            .find(|(t, _, _)| *t == Tool::Codex)
            .map(|(_, p, _)| p.clone())
            .unwrap_or_default();

        let (claude_cache, codex_cache) =
            tokio::join!(prefetch_claude_usage(), prefetch_codex_usage(&codex_profiles));

        let mut usage_caches: HashMap<Tool, HashMap<String, Vec<String>>> = HashMap::new();
        usage_caches.insert(Tool::Claude, claude_cache);
        usage_caches.insert(Tool::Codex, codex_cache);

        term.clear_last_lines(rendered_lines)?;

        let lines = build_usage_monitor_lines(&tool_profiles, &usage_caches);
        for line in &lines {
            term.write_line(line)?;
        }
        rendered_lines = lines.len();

        loop {
            tokio::select! {
                key_result = &mut key_task => {
                    let key = key_result
                        .map_err(|e| anyhow!("failed to read key input: {}", e))??;
                    match key {
                        Key::Escape | Key::Char('q') => {
                            term.clear_last_lines(rendered_lines)?;
                            return Ok(());
                        }
                        _ => {}
                    }
                    key_task = spawn_read_key_task();
                }
                _ = tokio::time::sleep(USAGE_MONITOR_REFRESH_INTERVAL) => {
                    break;
                }
            }
        }
    }
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

fn cmd_delete(tool_arg: Option<String>, profile_arg: Option<String>) -> Result<()> {
    let tool: Tool = match tool_arg {
        Some(t) => t.parse()?,
        None => {
            let Some(t) = select_tool()? else {
                return Ok(());
            };
            t
        }
    };

    let profile = match profile_arg {
        Some(p) => p,
        None => select_profile(tool)?,
    };

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

fn select_or_create_profile(tool: Tool) -> Result<String> {
    let mut profiles = tool.list_profiles()?;
    let create_label = "[Create new profile]";
    let mut items: Vec<&str> = profiles.iter().map(|s| s.as_str()).collect();
    items.push(create_label);

    let selection = Select::new()
        .with_prompt("Select profile to save credentials")
        .items(&items)
        .default(0)
        .interact()?;

    if selection == profiles.len() {
        let name: String = Input::new().with_prompt("Profile name").interact_text()?;
        profiles.push(name.clone());
        Ok(name)
    } else {
        Ok(profiles[selection].clone())
    }
}

fn cmd_login(tool_arg: Option<String>, profile_arg: Option<String>) -> Result<()> {
    let tool: Tool = match tool_arg {
        Some(t) => t.parse()?,
        None => {
            let Some(t) = select_tool()? else {
                return Ok(());
            };
            t
        }
    };

    let profile = match profile_arg {
        Some(p) => p,
        None => select_or_create_profile(tool)?,
    };

    let status = match tool {
        Tool::Claude => ProcessCommand::new("claude")
            .args(["auth", "login"])
            .status()?,
        Tool::Codex => ProcessCommand::new("codex").arg("login").status()?,
    };
    if !status.success() {
        return Err(anyhow!("login command failed"));
    }

    match tool {
        Tool::Claude => claude::profile::update(&profile)?,
        Tool::Codex => codex::profile::update(&profile)?,
    }

    fs::write(tool.current_file()?, format!("{}\n", profile))?;

    println!("Saved credentials to profile '{}' for {}", profile, tool);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::sleep;

    #[tokio::test]
    async fn prefetch_state_returns_ready_cache_immediately() {
        let mut cache = HashMap::new();
        cache.insert("p1".to_string(), vec!["ok".to_string()]);
        let mut state = PrefetchState::Ready(cache.clone());

        let actual = state
            .usage_cache_with_timeout(Duration::from_millis(1))
            .await;

        assert_eq!(actual, cache);
    }

    #[tokio::test]
    async fn prefetch_state_returns_empty_on_timeout_then_returns_result() {
        let handle = tokio::spawn(async {
            sleep(Duration::from_millis(30)).await;
            let mut cache = HashMap::new();
            cache.insert("p1".to_string(), vec!["ok".to_string()]);
            cache
        });
        let mut state = PrefetchState::Pending(handle);

        let first = state
            .usage_cache_with_timeout(Duration::from_millis(1))
            .await;
        assert!(first.is_empty());

        let second = state
            .usage_cache_with_timeout(Duration::from_millis(50))
            .await;
        assert_eq!(second.get("p1"), Some(&vec!["ok".to_string()]));
    }

    #[tokio::test]
    async fn prefetch_state_returns_empty_when_prefetch_task_fails() {
        let handle = tokio::spawn(async {
            panic!("boom");
            #[allow(unreachable_code)]
            HashMap::<String, Vec<String>>::new()
        });
        let mut state = PrefetchState::Pending(handle);

        let actual = state
            .usage_cache_with_timeout(Duration::from_millis(50))
            .await;

        assert!(actual.is_empty());
    }

    #[test]
    fn build_usage_monitor_lines_shows_no_profiles_when_empty() {
        let tool_profiles = vec![
            (Tool::Claude, vec![], None),
            (Tool::Codex, vec![], None),
        ];
        let usage_caches = HashMap::new();

        let lines = build_usage_monitor_lines(&tool_profiles, &usage_caches);

        let no_profiles_count = lines.iter().filter(|l| l.contains("(no profiles)")).count();
        assert_eq!(no_profiles_count, 2);
    }

    #[test]
    fn build_usage_monitor_lines_marks_current_profile_with_asterisk() {
        let tool_profiles = vec![(
            Tool::Claude,
            vec!["personal".to_string(), "work".to_string()],
            Some("personal".to_string()),
        )];
        let usage_caches = HashMap::new();

        let lines = build_usage_monitor_lines(&tool_profiles, &usage_caches);

        assert!(lines.iter().any(|l| l.contains("personal *")));
        assert!(!lines.iter().any(|l| l.contains("work *")));
    }

    #[test]
    fn build_usage_monitor_lines_shows_no_data_when_usage_missing() {
        let tool_profiles = vec![(
            Tool::Claude,
            vec!["personal".to_string()],
            Some("personal".to_string()),
        )];
        let usage_caches = HashMap::new();

        let lines = build_usage_monitor_lines(&tool_profiles, &usage_caches);

        assert!(lines.iter().any(|l| l.contains("(no data)")));
    }

    #[test]
    fn build_usage_monitor_lines_displays_usage_data() {
        let tool_profiles = vec![(
            Tool::Claude,
            vec!["personal".to_string()],
            Some("personal".to_string()),
        )];
        let mut claude_cache = HashMap::new();
        claude_cache.insert(
            "personal".to_string(),
            vec!["5-hour  60.0% used".to_string()],
        );
        let mut usage_caches = HashMap::new();
        usage_caches.insert(Tool::Claude, claude_cache);

        let lines = build_usage_monitor_lines(&tool_profiles, &usage_caches);

        assert!(lines.iter().any(|l| l.contains("60.0% used")));
        assert!(!lines.iter().any(|l| l.contains("(no data)")));
    }

    #[tokio::test]
    async fn refresh_usage_cache_updates_when_prefetch_completes() {
        let handle = tokio::spawn(async {
            sleep(Duration::from_millis(30)).await;
            let mut cache = HashMap::new();
            cache.insert("p1".to_string(), vec!["ok".to_string()]);
            cache
        });
        let mut state = PrefetchState::Pending(handle);
        let mut cache = HashMap::new();

        let first = refresh_usage_cache(&mut cache, &mut state).await;
        assert!(!first);
        assert!(cache.is_empty());

        sleep(Duration::from_millis(40)).await;

        let second = refresh_usage_cache(&mut cache, &mut state).await;
        assert!(second);
        assert_eq!(cache.get("p1"), Some(&vec!["ok".to_string()]));
    }
}
