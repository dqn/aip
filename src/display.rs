use chrono::{DateTime, Local, Utc};

const BAR_WIDTH: usize = 20;

pub fn render_bar(percent: f64) -> String {
    let filled = ((percent / 100.0) * BAR_WIDTH as f64).round() as usize;
    let filled = filled.min(BAR_WIDTH);
    let empty = BAR_WIDTH - filled;
    "\u{2588}".repeat(filled) + &"\u{2591}".repeat(empty)
}

pub fn format_reset_time(reset_utc: DateTime<Utc>) -> String {
    let local: DateTime<Local> = reset_utc.into();
    let now = Local::now();

    if local.date_naive() == now.date_naive() {
        local.format("%H:%M").to_string()
    } else {
        local.format("%b %d %H:%M").to_string()
    }
}
