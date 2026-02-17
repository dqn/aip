use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::PathBuf;

use anyhow::Result;
use chrono::{DateTime, Local, Utc};
use serde::Deserialize;

use crate::tool::Tool;

#[derive(Debug, Deserialize)]
struct SessionEntry {
    payload: Option<SessionPayload>,
}

#[derive(Debug, Deserialize)]
struct SessionPayload {
    rate_limits: Option<RateLimits>,
}

#[derive(Debug, Deserialize)]
pub struct RateLimits {
    pub primary: Option<RateWindow>,
    pub secondary: Option<RateWindow>,
}

#[derive(Debug, Deserialize)]
pub struct RateWindow {
    pub used_percent: f64,
    pub resets_at: i64,
}

impl RateWindow {
    pub fn resets_at_utc(&self) -> DateTime<Utc> {
        DateTime::from_timestamp(self.resets_at, 0).unwrap_or_default()
    }
}

fn find_session_files(
    since: Option<std::time::SystemTime>,
    cutoff: Option<std::time::SystemTime>,
) -> Result<Vec<PathBuf>> {
    let sessions_dir = Tool::Codex.home_dir()?.join("sessions");
    if !sessions_dir.exists() {
        return Ok(vec![]);
    }

    let today = Local::now().date_naive();

    // Search from today backwards up to 7 days
    for days_back in 0..7 {
        let date = today - chrono::Duration::days(days_back);
        let day_dir = sessions_dir
            .join(date.format("%Y").to_string())
            .join(date.format("%m").to_string())
            .join(date.format("%d").to_string());

        if !day_dir.exists() {
            continue;
        }

        let mut files: Vec<PathBuf> = std::fs::read_dir(&day_dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "jsonl"))
            .collect();

        if files.is_empty() {
            continue;
        }

        // Sort by modification time, newest first
        files.sort_by(|a, b| {
            let mtime_a = a.metadata().and_then(|m| m.modified()).ok();
            let mtime_b = b.metadata().and_then(|m| m.modified()).ok();
            mtime_b.cmp(&mtime_a)
        });

        // Filter: include files from current activation (>= since) or previous activations (<= cutoff)
        if since.is_some() || cutoff.is_some() {
            files.retain(|p| {
                p.metadata().and_then(|m| m.modified()).is_ok_and(|mtime| {
                    since.is_none_or(|s| mtime >= s) || cutoff.is_some_and(|c| mtime <= c)
                })
            });
        }

        return Ok(files);
    }

    Ok(vec![])
}

fn read_rate_limits_from_tail(path: &PathBuf) -> Result<Option<RateLimits>> {
    let file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();

    // Read last 64KB (should be more than enough)
    let read_size = size.min(65536);
    let mut reader = BufReader::new(file);
    reader.seek(SeekFrom::End(-(read_size as i64)))?;

    let mut buf = vec![0u8; read_size as usize];
    reader.read_exact(&mut buf)?;
    let content = String::from_utf8_lossy(&buf);

    // Parse lines in reverse to find the latest rate_limits
    for line in content.lines().rev() {
        if !line.contains("rate_limits") {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<SessionEntry>(line)
            && let Some(payload) = entry.payload
            && let Some(limits) = payload.rate_limits
        {
            return Ok(Some(limits));
        }
    }

    Ok(None)
}

pub fn fetch_usage(
    since: Option<std::time::SystemTime>,
    cutoff: Option<std::time::SystemTime>,
) -> Result<Option<RateLimits>> {
    let files = find_session_files(since, cutoff)?;
    for file in &files {
        if let Some(limits) = read_rate_limits_from_tail(file)? {
            return Ok(Some(limits));
        }
    }
    Ok(None)
}
