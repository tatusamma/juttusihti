//! Display formatting. Timestamps are stored in UTC and shown in local time.

use chrono::{DateTime, Local, TimeZone, Utc};

pub fn local(ts: i64) -> Option<DateTime<Local>> {
    Utc.timestamp_opt(ts, 0).single().map(|d| d.with_timezone(&Local))
}

/// Compact list date: time for today, day and month otherwise.
pub fn list_date(ts: i64) -> String {
    let Some(d) = local(ts) else { return String::new() };
    let now = Local::now();
    if d.date_naive() == now.date_naive() {
        d.format("%H:%M").to_string()
    } else if d.format("%Y").to_string() == now.format("%Y").to_string() {
        d.format("%-d.%-m.").to_string()
    } else {
        d.format("%-d.%-m.%Y").to_string()
    }
}

pub fn full_date(ts: i64) -> String {
    local(ts).map(|d| d.format("%-d.%-m.%Y %H:%M").to_string()).unwrap_or_default()
}

/// Splits a search snippet into (text, highlighted) segments.
pub fn snippet_segments(snippet: &str) -> Vec<(String, bool)> {
    use juttusihti_core::search::{HIGHLIGHT_END, HIGHLIGHT_START};
    let mut out = Vec::new();
    let mut current = String::new();
    let mut highlighted = false;
    for ch in snippet.chars() {
        if ch == HIGHLIGHT_START || ch == HIGHLIGHT_END {
            if !current.is_empty() {
                out.push((std::mem::take(&mut current), highlighted));
            }
            highlighted = ch == HIGHLIGHT_START;
        } else if ch == '\n' {
            current.push(' ');
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        out.push((current, highlighted));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_highlights() {
        let s = format!("a {}sää{} b", '\u{2}', '\u{3}');
        assert_eq!(
            snippet_segments(&s),
            vec![("a ".to_string(), false), ("sää".to_string(), true), (" b".to_string(), false)]
        );
    }
}
