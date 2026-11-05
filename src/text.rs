//! Small text helpers shared by the toast and the popup.

use chrono::{DateTime, Local, TimeZone};

/// One-line preview: collapses whitespace and truncates with an ellipsis.
pub fn one_line(text: &str, max_chars: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate(&collapsed, max_chars)
}

/// Multi-line preview: keeps the first `max_lines` non-empty lines.
pub fn lines(text: &str, max_lines: usize, max_chars: usize) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut more = false;
    for line in text.lines().map(|l| l.replace('\t', "    ")) {
        if line.trim().is_empty() {
            continue;
        }
        if out.len() == max_lines {
            more = true;
            break;
        }
        out.push(truncate(line.trim_end(), max_chars));
    }
    let mut joined = out.join("\n");
    if more && !joined.ends_with('…') {
        joined.push_str(" …");
    }
    joined
}

pub fn truncate(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((idx, _)) => format!("{}…", text[..idx].trim_end()),
        None => text.to_string(),
    }
}

/// "Today 14:32", "Yesterday 09:10", "Mon 3 Nov 14:32", "12 Mar 2025 14:32".
pub fn human_date(timestamp: i64) -> String {
    let Some(when) = Local.timestamp_opt(timestamp, 0).single() else {
        return String::new();
    };
    human_date_relative(when, Local::now())
}

fn human_date_relative(when: DateTime<Local>, now: DateTime<Local>) -> String {
    let days = (now.date_naive() - when.date_naive()).num_days();
    let time = when.format("%H:%M");
    match days {
        0 => format!("Today {time}"),
        1 => format!("Yesterday {time}"),
        2..=6 => format!("{} {time}", when.format("%a %-d %b")),
        _ if when.format("%Y").to_string() == now.format("%Y").to_string() => {
            format!("{} {time}", when.format("%-d %b"))
        }
        _ => format!("{} {time}", when.format("%-d %b %Y")),
    }
}

/// Case-insensitive "all words present" match.
pub fn matches(haystack_lower: &str, terms: &[String]) -> bool {
    terms.iter().all(|t| haystack_lower.contains(t.as_str()))
}

pub fn search_terms(query: &str) -> Vec<String> {
    query.split_whitespace().map(str::to_lowercase).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews() {
        assert_eq!(one_line("  a \n b\tc ", 10), "a b c");
        assert_eq!(one_line("abcdefghij", 4), "abcd…");
        assert_eq!(lines("x\n\n y\nz\nw", 2, 80), "x\n y …");
        assert_eq!(truncate("héllo", 2), "hé…");
    }

    #[test]
    fn dates() {
        let now = Local.with_ymd_and_hms(2026, 11, 5, 15, 0, 0).unwrap();
        let at = |d, h| Local.with_ymd_and_hms(2026, 11, d, h, 30, 0).unwrap();
        assert_eq!(human_date_relative(at(5, 9), now), "Today 09:30");
        assert_eq!(human_date_relative(at(4, 9), now), "Yesterday 09:30");
        assert_eq!(human_date_relative(at(2, 9), now), "Mon 2 Nov 09:30");
        let old = Local.with_ymd_and_hms(2025, 3, 12, 14, 32, 0).unwrap();
        assert_eq!(human_date_relative(old, now), "12 Mar 2025 14:32");
    }

    #[test]
    fn search() {
        let terms = search_terms("Foo  BAR");
        assert!(matches("xx bar yy foo", &terms));
        assert!(!matches("foo only", &terms));
        assert!(matches("anything", &[]));
    }
}
