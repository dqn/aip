use std::collections::{HashMap, HashSet};
use std::time::Duration;

use anyhow::Result;
use chrono::Local;
use console::{Key, Term};

use crate::claude;
use crate::claude::usage::RateLimitError;
use crate::codex;
use crate::codex::usage::RateLimits;
use crate::display::{DisplayMode, format_usage_line};
use crate::tool::Tool;

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[derive(Clone, Debug, PartialEq)]
struct ProfileUsageCache {
    lines: Vec<String>,
    plan_type: Option<String>,
    is_stale: bool,
}

type UsageCache = HashMap<String, ProfileUsageCache>;

enum DashboardMode {
    Normal,
    DeleteConfirm(usize),
}

enum DashboardAction {
    None,
    Render,
    Refresh,
    RefreshAfterDelete,
    Switch(Tool, String),
    Quit,
}

struct ScreenGuard<'a>(&'a Term);

impl Drop for ScreenGuard<'_> {
    fn drop(&mut self) {
        let _ = self.0.show_cursor();
        let _ = self.0.write_str("\x1b[?1049l");
    }
}

// The blocking thread will keep waiting on `read_key()` after the receiver is dropped,
// only exiting once the next keypress unblocks it. This is a known limitation of
// blocking terminal reads without timeout support in the `console` crate.
fn spawn_key_reader() -> tokio::sync::mpsc::UnboundedReceiver<std::io::Result<Key>> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::task::spawn_blocking(move || {
        let term = Term::stderr();
        loop {
            let key =
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| term.read_key())) {
                    Ok(result) => result,
                    Err(_) => {
                        eprintln!("key reader thread panicked");
                        break;
                    }
                };
            if tx.send(key).is_err() {
                break;
            }
        }
    });
    rx
}

fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
    }
}

// --- Usage fetching ---

fn format_retry_after(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 60 {
        format!("Rate limited (resets in {}m {}s)", secs / 60, secs % 60)
    } else if secs > 0 {
        format!("Rate limited (resets in {}s)", secs)
    } else {
        "Rate limited".to_string()
    }
}

async fn prefetch_claude_usage() -> UsageCache {
    let results = claude::usage::fetch_all_profiles_usage().await;

    results
        .into_iter()
        .map(|(profile, result)| {
            let entry = match result {
                Ok((usage, info)) => ProfileUsageCache {
                    lines: vec![
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
                    ],
                    plan_type: info.plan_type,
                    is_stale: false,
                },
                Err(e) => {
                    if let Some(rate_err) = e.downcast_ref::<RateLimitError>() {
                        let retry = rate_err.retry_after;
                        ProfileUsageCache {
                            lines: vec![format_retry_after(retry)],
                            plan_type: None,
                            // retry-after:0 may indicate unsupported plan;
                            // use stale cache to preserve previous usage data.
                            is_stale: retry.is_zero(),
                        }
                    } else {
                        ProfileUsageCache {
                            lines: vec![format!("Error: {}", e)],
                            plan_type: None,
                            is_stale: true,
                        }
                    }
                }
            };
            (profile, entry)
        })
        .collect()
}

/// Merge new Claude usage cache with old cache.
///
/// When a new entry is stale and old entry has valid (non-stale) data,
/// keep the old data but mark it as stale so the UI can show "(stale)".
fn merge_claude_cache(new_cache: UsageCache, old_cache: Option<&UsageCache>) -> UsageCache {
    let old = match old_cache {
        Some(c) => c,
        None => return new_cache,
    };
    new_cache
        .into_iter()
        .map(|(profile, new_entry)| {
            if new_entry.is_stale
                && let Some(old_entry) = old.get(&profile)
                && !old_entry.is_stale
            {
                return (
                    profile,
                    ProfileUsageCache {
                        is_stale: true,
                        ..old_entry.clone()
                    },
                );
            }
            (profile, new_entry)
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
                    primary.resets_at_utc(),
                    &DisplayMode::Left,
                ));
            }
            if let Some(secondary) = &limits.secondary {
                lines.push(format_usage_line(
                    "Weekly",
                    secondary.used_percent,
                    secondary.resets_at_utc(),
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

async fn prefetch_codex_usage(profiles: &[String]) -> UsageCache {
    // Sync active auth.json to current profile before fetching usage,
    // analogous to sync_keychain_to_current_profile for Claude.
    let _ = tokio::task::spawn_blocking(codex::profile::sync_auth_to_current_profile).await;

    let current = Tool::Codex.current_profile().ok().flatten();

    let mut handles = Vec::new();
    for p in profiles {
        let p = p.clone();
        let is_current = current.as_deref() == Some(p.as_str());
        handles.push(tokio::spawn(async move {
            let result = if is_current {
                codex::usage::fetch_usage().await
            } else {
                async {
                    let dir = Tool::Codex.profile_dir(&p)?;
                    let auth_path = dir.join("auth.json");
                    codex::usage::fetch_usage_from_auth(&auth_path).await
                }
                .await
            };
            (
                p,
                ProfileUsageCache {
                    lines: codex_usage_lines(result),
                    plan_type: None,
                    is_stale: false,
                },
            )
        }));
    }

    let mut results = HashMap::new();
    for handle in handles {
        if let Ok((p, entry)) = handle.await {
            results.insert(p, entry);
        }
    }
    results
}

// --- Dashboard ---

fn load_tool_profiles() -> Vec<(Tool, Vec<String>, Option<String>)> {
    Tool::ALL
        .iter()
        .map(|&t| {
            let profiles = t.list_profiles().unwrap_or_default();
            let current = t.current_profile().ok().flatten();
            (t, profiles, current)
        })
        .collect()
}

fn get_codex_profiles(tool_profiles: &[(Tool, Vec<String>, Option<String>)]) -> &[String] {
    tool_profiles
        .iter()
        .find(|(t, _, _)| *t == Tool::Codex)
        .map(|(_, p, _)| p.as_slice())
        .unwrap_or_default()
}

fn build_selectable_items(
    tool_profiles: &[(Tool, Vec<String>, Option<String>)],
) -> Vec<(Tool, String)> {
    tool_profiles
        .iter()
        .flat_map(|(tool, profiles, _)| profiles.iter().map(move |p| (*tool, p.clone())))
        .collect()
}

fn is_current_profile(
    tool_profiles: &[(Tool, Vec<String>, Option<String>)],
    tool: Tool,
    profile: &str,
) -> bool {
    tool_profiles
        .iter()
        .find(|(t, _, _)| *t == tool)
        .and_then(|(_, _, current)| current.as_deref())
        == Some(profile)
}

struct DashboardView<'a> {
    tool_profiles: &'a [(Tool, Vec<String>, Option<String>)],
    usage_caches: &'a HashMap<Tool, UsageCache>,
    pending_tools: &'a HashSet<Tool>,
    selectable_items: &'a [(Tool, String)],
    selected: usize,
    mode: &'a DashboardMode,
    spinner_frame: usize,
    status_message: Option<&'a str>,
}

impl DashboardView<'_> {
    fn build_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();

        let header = if self.pending_tools.is_empty() {
            let timestamp = Local::now().format("%H:%M:%S");
            format!("aip - Usage Monitor  Updated: {}", timestamp)
        } else {
            "aip - Usage Monitor  Refreshing...".to_string()
        };
        lines.push(header);
        lines.push(String::new());

        let mut item_idx = 0;

        for (tool, profiles, current) in self.tool_profiles {
            lines.push(tool.to_string());

            if profiles.is_empty() {
                lines.push("  (no profiles)".to_string());
            } else {
                let cache = self.usage_caches.get(tool);
                for profile in profiles {
                    let is_selected =
                        item_idx < self.selectable_items.len() && item_idx == self.selected;
                    let cursor = if is_selected { ">" } else { " " };
                    let marker = if current.as_deref() == Some(profile.as_str()) {
                        " \x1b[32m✓\x1b[0m"
                    } else {
                        ""
                    };
                    let entry = cache.and_then(|c| c.get(profile));
                    let plan_suffix = entry
                        .and_then(|e| e.plan_type.as_deref())
                        .map(|pt| format!(" ({})", capitalize_first(pt)))
                        .unwrap_or_default();
                    let stale_suffix = if entry.is_some_and(|e| e.is_stale) {
                        " \x1b[2m(stale)\x1b[0m"
                    } else {
                        ""
                    };
                    let spinner_suffix = if self.pending_tools.contains(tool) {
                        let spinner = SPINNER_FRAMES[self.spinner_frame % SPINNER_FRAMES.len()];
                        format!(" {}", spinner)
                    } else {
                        String::new()
                    };
                    let line = format!(
                        "{} {}{}{}{}{}",
                        cursor, profile, marker, plan_suffix, stale_suffix, spinner_suffix
                    );
                    if is_selected {
                        lines.push(format!("\x1b[1;36m{}\x1b[0m", line));
                    } else {
                        lines.push(line);
                    }

                    if let Some(entry) = entry {
                        for line in &entry.lines {
                            lines.push(format!("    {}", line));
                        }
                    } else if !self.pending_tools.contains(tool) {
                        lines.push("    (no data)".to_string());
                    }

                    item_idx += 1;
                }
            }

            lines.push(String::new());
        }

        if let Some(msg) = self.status_message {
            lines.push(format!("\x1b[31m{}\x1b[0m", msg));
            lines.push(String::new());
        }

        match self.mode {
            DashboardMode::Normal => {
                lines.push(
                    "[R] Refresh  [↑↓] Navigate  [Enter/Space] Switch  [BS/Del] Delete  [Shift+J/K] Reorder  [ESC/q] Quit"
                        .to_string(),
                );
            }
            DashboardMode::DeleteConfirm(idx) => {
                if let Some((tool, profile)) = self.selectable_items.get(*idx) {
                    lines.push(format!("Delete '{}' for {}? [y/n]", profile, tool));
                }
            }
        }

        lines
    }

    fn render(&self, term: &Term) -> Result<()> {
        term.write_str("\x1b[H")?;

        for line in &self.build_lines() {
            term.write_str(line)?;
            term.write_str("\x1b[K\n")?;
        }
        term.write_str("\x1b[J")?;

        Ok(())
    }
}

fn tool_item_range(tool: Tool, selectable_items: &[(Tool, String)]) -> std::ops::Range<usize> {
    let start = selectable_items
        .iter()
        .position(|(t, _)| *t == tool)
        .unwrap_or(0);
    let end = selectable_items
        .iter()
        .rposition(|(t, _)| *t == tool)
        .map(|i| i + 1)
        .unwrap_or(start);
    start..end
}

fn tool_profiles_for(
    tool: Tool,
    tool_profiles: &[(Tool, Vec<String>, Option<String>)],
) -> Vec<String> {
    tool_profiles
        .iter()
        .find(|(t, _, _)| *t == tool)
        .map(|(_, profiles, _)| profiles.clone())
        .unwrap_or_default()
}

fn handle_move(
    selected: &mut usize,
    selectable_items: &[(Tool, String)],
    tool_profiles: &[(Tool, Vec<String>, Option<String>)],
    direction: i32, // -1 for up, 1 for down
) -> DashboardAction {
    let (tool, _) = &selectable_items[*selected];
    let range = tool_item_range(*tool, selectable_items);

    // No-op if single profile or at boundary
    if range.len() <= 1 {
        return DashboardAction::None;
    }
    let target = *selected as i32 + direction;
    if target < range.start as i32 || target >= range.end as i32 {
        return DashboardAction::None;
    }

    let mut profiles = tool_profiles_for(*tool, tool_profiles);
    let local_idx = *selected - range.start;
    let target_local = target as usize - range.start;
    profiles.swap(local_idx, target_local);

    if tool.save_profile_order(&profiles).is_ok() {
        *selected = target as usize;
        DashboardAction::Refresh
    } else {
        DashboardAction::None
    }
}

fn switch_profile(tool: Tool, profile: &str) -> Result<()> {
    match tool {
        Tool::Claude => claude::profile::switch(profile),
        Tool::Codex => codex::profile::switch(profile),
    }
}

fn handle_dashboard_key(
    key: Key,
    selected: &mut usize,
    mode: &mut DashboardMode,
    selectable_items: &[(Tool, String)],
    tool_profiles: &[(Tool, Vec<String>, Option<String>)],
    status_message: &mut Option<String>,
) -> DashboardAction {
    if selectable_items.is_empty() {
        return match key {
            Key::Char('r') => DashboardAction::Refresh,
            Key::Escape | Key::Char('q') => DashboardAction::Quit,
            _ => DashboardAction::None,
        };
    }

    match mode {
        DashboardMode::Normal => match key {
            Key::ArrowUp => {
                *selected = selected.saturating_sub(1);
                DashboardAction::Render
            }
            Key::ArrowDown => {
                if *selected < selectable_items.len() - 1 {
                    *selected += 1;
                }
                DashboardAction::Render
            }
            Key::Enter | Key::Char(' ') => {
                let (tool, profile) = &selectable_items[*selected];
                if is_current_profile(tool_profiles, *tool, profile) {
                    return DashboardAction::None;
                }
                DashboardAction::Switch(*tool, profile.clone())
            }
            Key::Backspace | Key::Del => {
                let (tool, profile) = &selectable_items[*selected];
                if is_current_profile(tool_profiles, *tool, profile) {
                    return DashboardAction::None;
                }
                *mode = DashboardMode::DeleteConfirm(*selected);
                DashboardAction::Render
            }
            Key::Char('r') => DashboardAction::Refresh,
            Key::Char('K') => handle_move(selected, selectable_items, tool_profiles, -1),
            Key::Char('J') => handle_move(selected, selectable_items, tool_profiles, 1),
            Key::Escape | Key::Char('q') => DashboardAction::Quit,
            _ => DashboardAction::None,
        },
        DashboardMode::DeleteConfirm(idx) => {
            let idx = *idx;
            match key {
                Key::Char('y') => {
                    let (tool, profile) = &selectable_items[idx];
                    *mode = DashboardMode::Normal;
                    match tool.delete_profile(profile) {
                        Ok(()) => DashboardAction::RefreshAfterDelete,
                        Err(e) => {
                            *status_message = Some(format!("Failed to delete profile: {}", e));
                            DashboardAction::Render
                        }
                    }
                }
                Key::Char('n') | Key::Escape => {
                    *mode = DashboardMode::Normal;
                    DashboardAction::Render
                }
                _ => DashboardAction::None,
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_dashboard(
    term: &Term,
    tool_profiles: &[(Tool, Vec<String>, Option<String>)],
    usage_caches: &HashMap<Tool, UsageCache>,
    pending_tools: &HashSet<Tool>,
    selectable_items: &[(Tool, String)],
    selected: usize,
    mode: &DashboardMode,
    spinner_frame: usize,
    status_message: Option<&str>,
) -> Result<()> {
    DashboardView {
        tool_profiles,
        usage_caches,
        pending_tools,
        selectable_items,
        selected,
        mode,
        spinner_frame,
        status_message,
    }
    .render(term)
}

pub async fn cmd_dashboard() -> Result<()> {
    let term = Term::stderr();
    term.write_str("\x1b[?1049h")?;
    let _guard = ScreenGuard(&term);
    term.hide_cursor()?;

    let mut usage_caches: HashMap<Tool, UsageCache> = HashMap::new();
    let mut key_rx = spawn_key_reader();
    let mut selected: usize = 0;
    let mut mode = DashboardMode::Normal;
    let mut spinner_frame: usize = 0;
    let mut spinner_interval = tokio::time::interval(Duration::from_millis(80));
    let mut status_message: Option<String> = None;

    loop {
        let tool_profiles = load_tool_profiles();
        let codex_profiles = get_codex_profiles(&tool_profiles);
        let selectable_items = build_selectable_items(&tool_profiles);

        selected = selected.min(selectable_items.len().saturating_sub(1));

        let claude_future = prefetch_claude_usage();
        let codex_future = prefetch_codex_usage(codex_profiles);
        tokio::pin!(claude_future);
        tokio::pin!(codex_future);

        let mut pending_tools: HashSet<Tool> = HashSet::from([Tool::Claude, Tool::Codex]);

        render_dashboard(
            &term,
            &tool_profiles,
            &usage_caches,
            &pending_tools,
            &selectable_items,
            selected,
            &mode,
            spinner_frame,
            status_message.as_deref(),
        )?;

        loop {
            let mut should_render = false;

            tokio::select! {
                cache = &mut claude_future, if pending_tools.contains(&Tool::Claude) => {
                    let merged = merge_claude_cache(cache, usage_caches.get(&Tool::Claude));
                    usage_caches.insert(Tool::Claude, merged);
                    pending_tools.remove(&Tool::Claude);
                    should_render = true;
                }
                cache = &mut codex_future, if pending_tools.contains(&Tool::Codex) => {
                    usage_caches.insert(Tool::Codex, cache);
                    pending_tools.remove(&Tool::Codex);
                    should_render = true;
                }
                _ = spinner_interval.tick(), if !pending_tools.is_empty() => {
                    spinner_frame = spinner_frame.wrapping_add(1);
                    should_render = true;
                }
                _ = tokio::signal::ctrl_c() => {
                    return Ok(());
                }
                Some(key_result) = key_rx.recv() => {
                    let key = match key_result {
                        Ok(k) => k,
                        Err(_) => continue,
                    };
                    status_message = None;
                    match handle_dashboard_key(
                        key,
                        &mut selected,
                        &mut mode,
                        &selectable_items,
                        &tool_profiles,
                        &mut status_message,
                    ) {
                        DashboardAction::Quit => return Ok(()),
                        DashboardAction::Refresh => break,
                        DashboardAction::RefreshAfterDelete => {
                            selected = selected.saturating_sub(1);
                            break;
                        }
                        DashboardAction::Switch(tool, ref profile) => {
                            if tool == Tool::Claude
                                && let Ok(dir) = Tool::Claude.profile_dir(profile)
                            {
                                // Refresh only non-current profiles before switching.
                                // The current profile's token is managed by Claude Code.
                                let current =
                                    Tool::Claude.current_profile().ok().flatten();
                                if current.as_deref() != Some(profile.as_str())
                                    && let Err(e) =
                                        claude::usage::refresh_credentials_if_expired(
                                            &dir.join("credentials.json"),
                                        )
                                        .await
                                    {
                                        status_message = Some(format!(
                                            "Warning: credential refresh failed, tokens may be expired: {}",
                                            e,
                                        ));
                                    }
                            }
                            match switch_profile(tool, profile) {
                                Ok(()) => break,
                                Err(e) => {
                                    status_message = Some(format!(
                                        "Failed to switch profile: {}",
                                        e,
                                    ));
                                    should_render = true;
                                }
                            }
                        }
                        DashboardAction::Render => should_render = true,
                        DashboardAction::None => {}
                    }
                }
            }

            if should_render {
                render_dashboard(
                    &term,
                    &tool_profiles,
                    &usage_caches,
                    &pending_tools,
                    &selectable_items,
                    selected,
                    &mode,
                    spinner_frame,
                    status_message.as_deref(),
                )?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codex::usage::RateWindow;

    fn build_lines(
        tool_profiles: &[(Tool, Vec<String>, Option<String>)],
        usage_caches: &HashMap<Tool, UsageCache>,
        pending_tools: &HashSet<Tool>,
        selectable_items: &[(Tool, String)],
        selected: usize,
        mode: &DashboardMode,
        spinner_frame: usize,
    ) -> Vec<String> {
        DashboardView {
            tool_profiles,
            usage_caches,
            pending_tools,
            selectable_items,
            selected,
            mode,
            spinner_frame,
            status_message: None,
        }
        .build_lines()
    }

    fn make_entry(lines: Vec<String>, plan_type: Option<&str>) -> ProfileUsageCache {
        ProfileUsageCache {
            lines,
            plan_type: plan_type.map(String::from),
            is_stale: false,
        }
    }

    fn sample_tool_profiles() -> Vec<(Tool, Vec<String>, Option<String>)> {
        vec![
            (
                Tool::Claude,
                vec!["personal".to_string(), "work".to_string()],
                Some("personal".to_string()),
            ),
            (
                Tool::Codex,
                vec!["dev".to_string()],
                Some("dev".to_string()),
            ),
        ]
    }

    #[test]
    fn build_selectable_items_creates_flat_list() {
        let tool_profiles = sample_tool_profiles();
        let items = build_selectable_items(&tool_profiles);

        assert_eq!(items.len(), 3);
        assert_eq!(items[0], (Tool::Claude, "personal".to_string()));
        assert_eq!(items[1], (Tool::Claude, "work".to_string()));
        assert_eq!(items[2], (Tool::Codex, "dev".to_string()));
    }

    #[test]
    fn build_selectable_items_empty_when_no_profiles() {
        let tool_profiles = vec![(Tool::Claude, vec![], None), (Tool::Codex, vec![], None)];
        let items = build_selectable_items(&tool_profiles);

        assert!(items.is_empty());
    }

    #[test]
    fn is_current_profile_returns_true_for_current() {
        let tool_profiles = sample_tool_profiles();

        assert!(is_current_profile(&tool_profiles, Tool::Claude, "personal"));
        assert!(is_current_profile(&tool_profiles, Tool::Codex, "dev"));
    }

    #[test]
    fn is_current_profile_returns_false_for_non_current() {
        let tool_profiles = sample_tool_profiles();

        assert!(!is_current_profile(&tool_profiles, Tool::Claude, "work"));
    }

    #[test]
    fn build_dashboard_lines_shows_cursor_on_selected_profile() {
        let tool_profiles = vec![(
            Tool::Claude,
            vec!["personal".to_string(), "work".to_string()],
            Some("personal".to_string()),
        )];
        let selectable_items = build_selectable_items(&tool_profiles);

        let lines = build_lines(
            &tool_profiles,
            &HashMap::new(),
            &HashSet::new(),
            &selectable_items,
            1,
            &DashboardMode::Normal,
            0,
        );

        assert!(lines.iter().any(|l| l.starts_with("  personal")));
        assert!(lines.iter().any(|l| l.contains("> work")));
    }

    #[test]
    fn build_dashboard_lines_shows_no_profiles_when_empty() {
        let tool_profiles = vec![(Tool::Claude, vec![], None), (Tool::Codex, vec![], None)];
        let selectable_items = build_selectable_items(&tool_profiles);

        let lines = build_lines(
            &tool_profiles,
            &HashMap::new(),
            &HashSet::new(),
            &selectable_items,
            0,
            &DashboardMode::Normal,
            0,
        );

        let no_profiles_count = lines.iter().filter(|l| l.contains("(no profiles)")).count();
        assert_eq!(no_profiles_count, 2);
    }

    #[test]
    fn build_dashboard_lines_marks_current_profile_with_asterisk() {
        let tool_profiles = vec![(
            Tool::Claude,
            vec!["personal".to_string(), "work".to_string()],
            Some("personal".to_string()),
        )];
        let selectable_items = build_selectable_items(&tool_profiles);

        let lines = build_lines(
            &tool_profiles,
            &HashMap::new(),
            &HashSet::new(),
            &selectable_items,
            0,
            &DashboardMode::Normal,
            0,
        );

        assert!(
            lines
                .iter()
                .any(|l| l.contains("personal \x1b[32m✓\x1b[0m"))
        );
        assert!(!lines.iter().any(|l| l.contains("work \x1b[32m✓\x1b[0m")));
    }

    #[test]
    fn build_dashboard_lines_shows_no_data_when_usage_missing() {
        let tool_profiles = vec![(
            Tool::Claude,
            vec!["personal".to_string()],
            Some("personal".to_string()),
        )];
        let selectable_items = build_selectable_items(&tool_profiles);

        let lines = build_lines(
            &tool_profiles,
            &HashMap::new(),
            &HashSet::new(),
            &selectable_items,
            0,
            &DashboardMode::Normal,
            0,
        );

        assert!(lines.iter().any(|l| l.contains("(no data)")));
    }

    #[test]
    fn build_dashboard_lines_displays_usage_data() {
        let tool_profiles = vec![(
            Tool::Claude,
            vec!["personal".to_string()],
            Some("personal".to_string()),
        )];
        let mut claude_cache: UsageCache = HashMap::new();
        claude_cache.insert(
            "personal".to_string(),
            make_entry(vec!["5-hour  60.0% used".to_string()], None),
        );
        let mut usage_caches = HashMap::new();
        usage_caches.insert(Tool::Claude, claude_cache);
        let selectable_items = build_selectable_items(&tool_profiles);

        let lines = build_lines(
            &tool_profiles,
            &usage_caches,
            &HashSet::new(),
            &selectable_items,
            0,
            &DashboardMode::Normal,
            0,
        );

        assert!(lines.iter().any(|l| l.contains("60.0% used")));
        assert!(!lines.iter().any(|l| l.contains("(no data)")));
    }

    #[test]
    fn build_dashboard_lines_shows_spinner_on_profile_line_when_pending_with_cache() {
        let tool_profiles = vec![(
            Tool::Claude,
            vec!["personal".to_string()],
            Some("personal".to_string()),
        )];
        let mut claude_cache: UsageCache = HashMap::new();
        claude_cache.insert(
            "personal".to_string(),
            make_entry(vec!["5-hour  60.0% used".to_string()], None),
        );
        let mut usage_caches = HashMap::new();
        usage_caches.insert(Tool::Claude, claude_cache);
        let pending_tools = HashSet::from([Tool::Claude]);
        let selectable_items = build_selectable_items(&tool_profiles);

        let lines = build_lines(
            &tool_profiles,
            &usage_caches,
            &pending_tools,
            &selectable_items,
            0,
            &DashboardMode::Normal,
            0,
        );

        // Spinner appears on the profile name line
        let profile_line = lines.iter().find(|l| l.contains("personal")).unwrap();
        assert!(profile_line.contains(SPINNER_FRAMES[0]));
        // Cached data is still shown
        assert!(lines.iter().any(|l| l.contains("60.0% used")));
    }

    #[test]
    fn build_dashboard_lines_shows_spinner_when_tool_pending() {
        let tool_profiles = vec![(
            Tool::Claude,
            vec!["personal".to_string()],
            Some("personal".to_string()),
        )];
        let pending_tools = HashSet::from([Tool::Claude]);
        let selectable_items = build_selectable_items(&tool_profiles);

        let lines = build_lines(
            &tool_profiles,
            &HashMap::new(),
            &pending_tools,
            &selectable_items,
            0,
            &DashboardMode::Normal,
            0,
        );

        assert!(lines.iter().any(|l| l.contains(SPINNER_FRAMES[0])));
        assert!(!lines.iter().any(|l| l.contains("(no data)")));
    }

    #[test]
    fn build_dashboard_lines_spinner_advances_with_frame() {
        let tool_profiles = vec![(
            Tool::Claude,
            vec!["personal".to_string()],
            Some("personal".to_string()),
        )];
        let pending_tools = HashSet::from([Tool::Claude]);
        let selectable_items = build_selectable_items(&tool_profiles);

        let lines_frame_0 = build_lines(
            &tool_profiles,
            &HashMap::new(),
            &pending_tools,
            &selectable_items,
            0,
            &DashboardMode::Normal,
            0,
        );
        let lines_frame_1 = build_lines(
            &tool_profiles,
            &HashMap::new(),
            &pending_tools,
            &selectable_items,
            0,
            &DashboardMode::Normal,
            1,
        );

        let spinner_line_0 = lines_frame_0.iter().find(|l| l.contains(SPINNER_FRAMES[0]));
        let spinner_line_1 = lines_frame_1.iter().find(|l| l.contains(SPINNER_FRAMES[1]));
        assert!(spinner_line_0.is_some());
        assert!(spinner_line_1.is_some());
    }

    #[test]
    fn build_dashboard_lines_header_shows_refreshing_when_pending() {
        let tool_profiles = vec![(Tool::Claude, vec![], None)];
        let pending_tools = HashSet::from([Tool::Claude]);
        let selectable_items = build_selectable_items(&tool_profiles);

        let lines = build_lines(
            &tool_profiles,
            &HashMap::new(),
            &pending_tools,
            &selectable_items,
            0,
            &DashboardMode::Normal,
            0,
        );

        assert!(lines[0].contains("Refreshing..."));
        assert!(!lines[0].contains("Updated:"));
    }

    #[test]
    fn build_dashboard_lines_header_shows_updated_when_not_pending() {
        let tool_profiles = vec![(Tool::Claude, vec![], None)];
        let selectable_items = build_selectable_items(&tool_profiles);

        let lines = build_lines(
            &tool_profiles,
            &HashMap::new(),
            &HashSet::new(),
            &selectable_items,
            0,
            &DashboardMode::Normal,
            0,
        );

        assert!(lines[0].contains("Updated:"));
        assert!(!lines[0].contains("Refreshing..."));
    }

    #[test]
    fn build_dashboard_lines_shows_plan_type() {
        let tool_profiles = vec![(
            Tool::Claude,
            vec!["personal".to_string()],
            Some("personal".to_string()),
        )];
        let mut claude_cache: UsageCache = HashMap::new();
        claude_cache.insert(
            "personal".to_string(),
            make_entry(vec!["5-hour  60.0% used".to_string()], Some("pro")),
        );
        let mut usage_caches = HashMap::new();
        usage_caches.insert(Tool::Claude, claude_cache);
        let selectable_items = build_selectable_items(&tool_profiles);

        let lines = build_lines(
            &tool_profiles,
            &usage_caches,
            &HashSet::new(),
            &selectable_items,
            0,
            &DashboardMode::Normal,
            0,
        );

        assert!(
            lines
                .iter()
                .any(|l| l.contains("personal \x1b[32m✓\x1b[0m (Pro)"))
        );
    }

    #[test]
    fn build_dashboard_lines_hides_plan_type_when_none() {
        let tool_profiles = vec![(
            Tool::Claude,
            vec!["personal".to_string()],
            Some("personal".to_string()),
        )];
        let mut claude_cache: UsageCache = HashMap::new();
        claude_cache.insert(
            "personal".to_string(),
            make_entry(vec!["5-hour  60.0% used".to_string()], None),
        );
        let mut usage_caches = HashMap::new();
        usage_caches.insert(Tool::Claude, claude_cache);
        let selectable_items = build_selectable_items(&tool_profiles);

        let lines = build_lines(
            &tool_profiles,
            &usage_caches,
            &HashSet::new(),
            &selectable_items,
            0,
            &DashboardMode::Normal,
            0,
        );

        let profile_line = lines
            .iter()
            .find(|l| l.contains("personal \x1b[32m✓\x1b[0m"))
            .unwrap();
        assert!(!profile_line.contains("("));
    }

    #[test]
    fn build_dashboard_lines_footer_shows_keybindings_in_normal_mode() {
        let tool_profiles = vec![(Tool::Claude, vec!["p".to_string()], Some("p".to_string()))];
        let selectable_items = build_selectable_items(&tool_profiles);

        let lines = build_lines(
            &tool_profiles,
            &HashMap::new(),
            &HashSet::new(),
            &selectable_items,
            0,
            &DashboardMode::Normal,
            0,
        );

        let footer = lines.last().unwrap();
        assert!(footer.contains("Navigate"));
        assert!(footer.contains("Reorder"));
        assert!(footer.contains("Switch"));
        assert!(footer.contains("Delete"));
        assert!(footer.contains("Quit"));
    }

    #[test]
    fn build_dashboard_lines_footer_shows_confirm_in_delete_mode() {
        let tool_profiles = vec![(
            Tool::Claude,
            vec!["personal".to_string(), "work".to_string()],
            Some("personal".to_string()),
        )];
        let selectable_items = build_selectable_items(&tool_profiles);

        let lines = build_lines(
            &tool_profiles,
            &HashMap::new(),
            &HashSet::new(),
            &selectable_items,
            1,
            &DashboardMode::DeleteConfirm(1),
            0,
        );

        let footer = lines.last().unwrap();
        assert!(footer.contains("Delete 'work' for Claude Code? [y/n]"));
    }

    #[test]
    fn capitalize_first_capitalizes_first_char() {
        assert_eq!(capitalize_first("pro"), "Pro");
        assert_eq!(capitalize_first(""), "");
        assert_eq!(capitalize_first("Pro"), "Pro");
        assert_eq!(capitalize_first("日本語"), "日本語");
    }

    #[test]
    fn handle_dashboard_key_navigates_with_arrow_keys() {
        let tool_profiles = sample_tool_profiles();
        let selectable_items = build_selectable_items(&tool_profiles);
        let mut selected = 0;
        let mut mode = DashboardMode::Normal;

        let action = handle_dashboard_key(
            Key::ArrowDown,
            &mut selected,
            &mut mode,
            &selectable_items,
            &tool_profiles,
            &mut None,
        );
        assert!(matches!(action, DashboardAction::Render));
        assert_eq!(selected, 1);

        let action = handle_dashboard_key(
            Key::ArrowUp,
            &mut selected,
            &mut mode,
            &selectable_items,
            &tool_profiles,
            &mut None,
        );
        assert!(matches!(action, DashboardAction::Render));
        assert_eq!(selected, 0);
    }

    #[test]
    fn handle_dashboard_key_does_not_navigate_past_bounds() {
        let tool_profiles = sample_tool_profiles();
        let selectable_items = build_selectable_items(&tool_profiles);
        let mut selected = 0;
        let mut mode = DashboardMode::Normal;

        handle_dashboard_key(
            Key::ArrowUp,
            &mut selected,
            &mut mode,
            &selectable_items,
            &tool_profiles,
            &mut None,
        );
        assert_eq!(selected, 0);

        selected = selectable_items.len() - 1;
        handle_dashboard_key(
            Key::ArrowDown,
            &mut selected,
            &mut mode,
            &selectable_items,
            &tool_profiles,
            &mut None,
        );
        assert_eq!(selected, selectable_items.len() - 1);
    }

    #[test]
    fn handle_dashboard_key_enter_on_current_profile_does_nothing() {
        let tool_profiles = sample_tool_profiles();
        let selectable_items = build_selectable_items(&tool_profiles);
        let mut selected = 0; // "personal" is current for Claude
        let mut mode = DashboardMode::Normal;

        let action = handle_dashboard_key(
            Key::Enter,
            &mut selected,
            &mut mode,
            &selectable_items,
            &tool_profiles,
            &mut None,
        );
        assert!(matches!(action, DashboardAction::None));
    }

    #[test]
    fn handle_dashboard_key_backspace_on_current_profile_does_nothing() {
        let tool_profiles = sample_tool_profiles();
        let selectable_items = build_selectable_items(&tool_profiles);
        let mut selected = 0; // "personal" is current for Claude
        let mut mode = DashboardMode::Normal;

        let action = handle_dashboard_key(
            Key::Backspace,
            &mut selected,
            &mut mode,
            &selectable_items,
            &tool_profiles,
            &mut None,
        );
        assert!(matches!(action, DashboardAction::None));
    }

    #[test]
    fn handle_dashboard_key_backspace_enters_delete_confirm() {
        let tool_profiles = sample_tool_profiles();
        let selectable_items = build_selectable_items(&tool_profiles);
        let mut selected = 1; // "work" is NOT current for Claude
        let mut mode = DashboardMode::Normal;

        let action = handle_dashboard_key(
            Key::Backspace,
            &mut selected,
            &mut mode,
            &selectable_items,
            &tool_profiles,
            &mut None,
        );
        assert!(matches!(action, DashboardAction::Render));
        assert!(matches!(mode, DashboardMode::DeleteConfirm(1)));
    }

    #[test]
    fn handle_dashboard_key_escape_quits() {
        let tool_profiles = sample_tool_profiles();
        let selectable_items = build_selectable_items(&tool_profiles);
        let mut selected = 0;
        let mut mode = DashboardMode::Normal;

        let action = handle_dashboard_key(
            Key::Escape,
            &mut selected,
            &mut mode,
            &selectable_items,
            &tool_profiles,
            &mut None,
        );
        assert!(matches!(action, DashboardAction::Quit));
    }

    #[test]
    fn handle_dashboard_key_q_quits() {
        let tool_profiles = sample_tool_profiles();
        let selectable_items = build_selectable_items(&tool_profiles);
        let mut selected = 0;
        let mut mode = DashboardMode::Normal;

        let action = handle_dashboard_key(
            Key::Char('q'),
            &mut selected,
            &mut mode,
            &selectable_items,
            &tool_profiles,
            &mut None,
        );
        assert!(matches!(action, DashboardAction::Quit));
    }

    #[test]
    fn handle_dashboard_key_delete_confirm_n_cancels() {
        let tool_profiles = sample_tool_profiles();
        let selectable_items = build_selectable_items(&tool_profiles);
        let mut selected = 1;
        let mut mode = DashboardMode::DeleteConfirm(1);

        let action = handle_dashboard_key(
            Key::Char('n'),
            &mut selected,
            &mut mode,
            &selectable_items,
            &tool_profiles,
            &mut None,
        );
        assert!(matches!(action, DashboardAction::Render));
        assert!(matches!(mode, DashboardMode::Normal));
    }

    #[test]
    fn handle_dashboard_key_delete_confirm_escape_cancels() {
        let tool_profiles = sample_tool_profiles();
        let selectable_items = build_selectable_items(&tool_profiles);
        let mut selected = 1;
        let mut mode = DashboardMode::DeleteConfirm(1);

        let action = handle_dashboard_key(
            Key::Escape,
            &mut selected,
            &mut mode,
            &selectable_items,
            &tool_profiles,
            &mut None,
        );
        assert!(matches!(action, DashboardAction::Render));
        assert!(matches!(mode, DashboardMode::Normal));
    }

    #[test]
    fn tool_item_range_returns_correct_range() {
        let tool_profiles = sample_tool_profiles();
        let items = build_selectable_items(&tool_profiles);

        let claude_range = tool_item_range(Tool::Claude, &items);
        assert_eq!(claude_range, 0..2);

        let codex_range = tool_item_range(Tool::Codex, &items);
        assert_eq!(codex_range, 2..3);
    }

    #[test]
    fn tool_item_range_returns_empty_for_missing_tool() {
        let tool_profiles = vec![(
            Tool::Claude,
            vec!["personal".to_string()],
            Some("personal".to_string()),
        )];
        let items = build_selectable_items(&tool_profiles);

        let codex_range = tool_item_range(Tool::Codex, &items);
        assert_eq!(codex_range.len(), 0);
    }

    #[test]
    fn tool_profiles_for_returns_profiles() {
        let tool_profiles = sample_tool_profiles();

        let claude_profiles = tool_profiles_for(Tool::Claude, &tool_profiles);
        assert_eq!(claude_profiles, vec!["personal", "work"]);

        let codex_profiles = tool_profiles_for(Tool::Codex, &tool_profiles);
        assert_eq!(codex_profiles, vec!["dev"]);
    }

    #[test]
    fn tool_profiles_for_returns_empty_for_missing_tool() {
        let tool_profiles = vec![(Tool::Claude, vec!["a".to_string()], None)];

        let codex_profiles = tool_profiles_for(Tool::Codex, &tool_profiles);
        assert!(codex_profiles.is_empty());
    }

    #[test]
    fn handle_dashboard_key_move_down_updates_selected() {
        let tool_profiles = sample_tool_profiles();
        let selectable_items = build_selectable_items(&tool_profiles);
        let mut selected = 0; // "personal" for Claude
        let mut mode = DashboardMode::Normal;

        let action = handle_dashboard_key(
            Key::Char('J'),
            &mut selected,
            &mut mode,
            &selectable_items,
            &tool_profiles,
            &mut None,
        );
        // handle_move calls save_profile_order which may fail in test env,
        // but selected should still be updated on success (Reload) or unchanged (None).
        match action {
            DashboardAction::Refresh => assert_eq!(selected, 1),
            _ => assert_eq!(selected, 0),
        }
    }

    #[test]
    fn handle_dashboard_key_move_up_updates_selected() {
        let tool_profiles = sample_tool_profiles();
        let selectable_items = build_selectable_items(&tool_profiles);
        let mut selected = 1; // "work" for Claude
        let mut mode = DashboardMode::Normal;

        let action = handle_dashboard_key(
            Key::Char('K'),
            &mut selected,
            &mut mode,
            &selectable_items,
            &tool_profiles,
            &mut None,
        );
        match action {
            DashboardAction::Refresh => assert_eq!(selected, 0),
            _ => assert_eq!(selected, 1),
        }
    }

    #[test]
    fn handle_dashboard_key_r_refreshes() {
        let tool_profiles = sample_tool_profiles();
        let selectable_items = build_selectable_items(&tool_profiles);
        let mut selected = 0;
        let mut mode = DashboardMode::Normal;

        let action = handle_dashboard_key(
            Key::Char('r'),
            &mut selected,
            &mut mode,
            &selectable_items,
            &tool_profiles,
            &mut None,
        );
        assert!(matches!(action, DashboardAction::Refresh));
    }

    #[test]
    fn handle_dashboard_key_quits_when_no_selectable_items() {
        let tool_profiles = vec![(Tool::Claude, vec![], None)];
        let selectable_items = build_selectable_items(&tool_profiles);
        let mut selected = 0;
        let mut mode = DashboardMode::Normal;

        let action = handle_dashboard_key(
            Key::Escape,
            &mut selected,
            &mut mode,
            &selectable_items,
            &tool_profiles,
            &mut None,
        );
        assert!(matches!(action, DashboardAction::Quit));

        let action = handle_dashboard_key(
            Key::ArrowDown,
            &mut selected,
            &mut mode,
            &selectable_items,
            &tool_profiles,
            &mut None,
        );
        assert!(matches!(action, DashboardAction::None));
    }

    #[test]
    fn handle_dashboard_key_refreshes_when_no_selectable_items() {
        let tool_profiles = vec![(Tool::Claude, vec![], None)];
        let selectable_items = build_selectable_items(&tool_profiles);
        let mut selected = 0;
        let mut mode = DashboardMode::Normal;

        let action = handle_dashboard_key(
            Key::Char('r'),
            &mut selected,
            &mut mode,
            &selectable_items,
            &tool_profiles,
            &mut None,
        );
        assert!(matches!(action, DashboardAction::Refresh));
    }

    #[test]
    fn merge_claude_cache_keeps_old_data_when_new_is_stale() {
        let old: UsageCache = HashMap::from([(
            "main".to_string(),
            ProfileUsageCache {
                lines: vec!["5-hour  40.0% used".to_string()],
                plan_type: Some("pro".to_string()),
                is_stale: false,
            },
        )]);
        let new: UsageCache = HashMap::from([(
            "main".to_string(),
            ProfileUsageCache {
                lines: vec!["Rate limited".to_string()],
                plan_type: None,
                is_stale: true,
            },
        )]);

        let merged = merge_claude_cache(new, Some(&old));
        let entry = &merged["main"];
        assert!(entry.is_stale);
        assert_eq!(entry.lines, vec!["5-hour  40.0% used"]);
        assert_eq!(entry.plan_type, Some("pro".to_string()));
    }

    #[test]
    fn merge_claude_cache_uses_new_data_when_not_stale() {
        let old: UsageCache = HashMap::from([(
            "main".to_string(),
            ProfileUsageCache {
                lines: vec!["5-hour  40.0% used".to_string()],
                plan_type: Some("pro".to_string()),
                is_stale: false,
            },
        )]);
        let new: UsageCache = HashMap::from([(
            "main".to_string(),
            ProfileUsageCache {
                lines: vec!["5-hour  50.0% used".to_string()],
                plan_type: Some("pro".to_string()),
                is_stale: false,
            },
        )]);

        let merged = merge_claude_cache(new, Some(&old));
        let entry = &merged["main"];
        assert!(!entry.is_stale);
        assert_eq!(entry.lines, vec!["5-hour  50.0% used"]);
    }

    #[test]
    fn merge_claude_cache_uses_fallback_when_no_old_data() {
        let new: UsageCache = HashMap::from([(
            "main".to_string(),
            ProfileUsageCache {
                lines: vec!["Rate limited".to_string()],
                plan_type: None,
                is_stale: true,
            },
        )]);

        let merged = merge_claude_cache(new, None);
        let entry = &merged["main"];
        assert!(entry.is_stale);
        assert_eq!(entry.lines, vec!["Rate limited"]);
    }

    #[test]
    fn merge_claude_cache_keeps_stale_fallback_when_old_also_stale() {
        let old: UsageCache = HashMap::from([(
            "main".to_string(),
            ProfileUsageCache {
                lines: vec!["5-hour  40.0% used".to_string()],
                plan_type: Some("pro".to_string()),
                is_stale: true,
            },
        )]);
        let new: UsageCache = HashMap::from([(
            "main".to_string(),
            ProfileUsageCache {
                lines: vec!["Rate limited".to_string()],
                plan_type: None,
                is_stale: true,
            },
        )]);

        let merged = merge_claude_cache(new, Some(&old));
        let entry = &merged["main"];
        assert!(entry.is_stale);
        // Old was also stale, so keep old cached data
        assert_eq!(entry.lines, vec!["Rate limited"]);
    }

    #[test]
    fn build_dashboard_lines_shows_stale_indicator() {
        let tool_profiles = vec![(
            Tool::Claude,
            vec!["main".to_string()],
            Some("main".to_string()),
        )];
        let mut claude_cache: UsageCache = HashMap::new();
        claude_cache.insert(
            "main".to_string(),
            ProfileUsageCache {
                lines: vec!["5-hour  40.0% used".to_string()],
                plan_type: Some("pro".to_string()),
                is_stale: true,
            },
        );
        let mut usage_caches = HashMap::new();
        usage_caches.insert(Tool::Claude, claude_cache);
        let selectable_items = build_selectable_items(&tool_profiles);

        let lines = build_lines(
            &tool_profiles,
            &usage_caches,
            &HashSet::new(),
            &selectable_items,
            0,
            &DashboardMode::Normal,
            0,
        );

        assert!(lines.iter().any(|l| l.contains("(stale)")));
        assert!(lines.iter().any(|l| l.contains("(Pro)")));
    }

    #[test]
    fn build_dashboard_lines_hides_stale_when_not_stale() {
        let tool_profiles = vec![(
            Tool::Claude,
            vec!["main".to_string()],
            Some("main".to_string()),
        )];
        let mut claude_cache: UsageCache = HashMap::new();
        claude_cache.insert(
            "main".to_string(),
            ProfileUsageCache {
                lines: vec!["5-hour  40.0% used".to_string()],
                plan_type: Some("pro".to_string()),
                is_stale: false,
            },
        );
        let mut usage_caches = HashMap::new();
        usage_caches.insert(Tool::Claude, claude_cache);
        let selectable_items = build_selectable_items(&tool_profiles);

        let lines = build_lines(
            &tool_profiles,
            &usage_caches,
            &HashSet::new(),
            &selectable_items,
            0,
            &DashboardMode::Normal,
            0,
        );

        assert!(!lines.iter().any(|l| l.contains("(stale)")));
    }

    // --- format_retry_after tests ---

    #[test]
    fn format_retry_after_minutes_and_seconds() {
        let result = format_retry_after(Duration::from_secs(125));
        assert_eq!(result, "Rate limited (resets in 2m 5s)");
    }

    #[test]
    fn format_retry_after_seconds_only() {
        let result = format_retry_after(Duration::from_secs(30));
        assert_eq!(result, "Rate limited (resets in 30s)");
    }

    #[test]
    fn format_retry_after_zero() {
        let result = format_retry_after(Duration::from_secs(0));
        assert_eq!(result, "Rate limited");
    }

    #[test]
    fn format_retry_after_boundary_60s() {
        let result = format_retry_after(Duration::from_secs(60));
        assert_eq!(result, "Rate limited (resets in 1m 0s)");
    }

    // --- codex_usage_lines tests ---

    #[test]
    fn codex_usage_lines_with_both_windows() {
        let limits = RateLimits {
            primary: Some(RateWindow {
                used_percent: 50.0,
                resets_at: 1700000000,
            }),
            secondary: Some(RateWindow {
                used_percent: 30.0,
                resets_at: 1700100000,
            }),
        };
        let lines = codex_usage_lines(Ok(Some(limits)));
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("5-hour"));
        assert!(lines[1].contains("Weekly"));
    }

    #[test]
    fn codex_usage_lines_none_returns_no_data() {
        let lines = codex_usage_lines(Ok(None));
        assert_eq!(lines, vec!["No usage data available"]);
    }

    #[test]
    fn codex_usage_lines_error_starts_with_error() {
        let lines = codex_usage_lines(Err(anyhow::anyhow!("connection failed")));
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("Error: "));
        assert!(lines[0].contains("connection failed"));
    }

    // --- status_message rendering test ---

    #[test]
    fn build_dashboard_lines_shows_status_message_in_red_before_footer() {
        let tool_profiles = vec![(
            Tool::Claude,
            vec!["personal".to_string()],
            Some("personal".to_string()),
        )];
        let selectable_items = build_selectable_items(&tool_profiles);

        let view = DashboardView {
            tool_profiles: &tool_profiles,
            usage_caches: &HashMap::new(),
            pending_tools: &HashSet::new(),
            selectable_items: &selectable_items,
            selected: 0,
            mode: &DashboardMode::Normal,
            spinner_frame: 0,
            status_message: Some("Failed to delete profile: not found"),
        };
        let lines = view.build_lines();

        // Status message should appear in red (wrapped with \x1b[31m...\x1b[0m)
        let status_line = lines
            .iter()
            .find(|l| l.contains("Failed to delete profile"))
            .expect("status message should be present");
        assert!(status_line.starts_with("\x1b[31m"));
        assert!(status_line.ends_with("\x1b[0m"));

        // Status message should appear before the footer (last line)
        let footer = lines.last().unwrap();
        assert!(footer.contains("Navigate"));
        let status_idx = lines
            .iter()
            .position(|l| l.contains("Failed to delete"))
            .unwrap();
        let footer_idx = lines.len() - 1;
        assert!(status_idx < footer_idx);
    }

    // --- delete-confirm 'y' test ---

    #[test]
    fn handle_dashboard_key_delete_confirm_y_captures_error_in_status_message() {
        let tool_profiles = sample_tool_profiles();
        let selectable_items = build_selectable_items(&tool_profiles);
        let mut selected = 1; // "work" is NOT current for Claude
        let mut mode = DashboardMode::DeleteConfirm(1);
        let mut status_message: Option<String> = None;

        let action = handle_dashboard_key(
            Key::Char('y'),
            &mut selected,
            &mut mode,
            &selectable_items,
            &tool_profiles,
            &mut status_message,
        );

        // delete_profile will fail in test environment (no real profile dirs),
        // so the error is captured in status_message and mode resets to Normal.
        assert!(matches!(mode, DashboardMode::Normal));
        match action {
            DashboardAction::Render => {
                // Delete failed, error captured in status_message
                let msg = status_message
                    .as_ref()
                    .expect("status_message should be set on failure");
                assert!(msg.starts_with("Failed to delete profile: "));
            }
            DashboardAction::RefreshAfterDelete => {
                // If the delete somehow succeeded (unlikely in test), that's fine too
            }
            _ => panic!("expected Render or RefreshAfterDelete, got other action"),
        }
    }
}
