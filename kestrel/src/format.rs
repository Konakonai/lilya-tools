//! Number and time formatting shared by the CLI and the UI.

use chrono::{Local, NaiveDate};

pub fn usd(v: f64) -> String {
    let a = v.abs();
    if a == 0.0 {
        "$0".into()
    } else if a < 0.01 {
        "<$0.01".into()
    } else if a < 100.0 {
        format!("${v:.2}")
    } else if a < 10_000.0 {
        format!("${}", group(v.round() as i64))
    } else {
        format!("${:.1}k", v / 1000.0)
    }
}

/// USD always with cents, for tables.
pub fn usd_exact(v: f64) -> String {
    if v != 0.0 && v.abs() < 0.01 {
        return format!("${v:.4}");
    }
    let cents = (v * 100.0).round() as i64;
    format!("${}.{:02}", group(cents / 100), (cents % 100).abs())
}

pub fn group(n: i64) -> String {
    let s = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    if n < 0 { format!("-{out}") } else { out }
}

pub fn tokens(n: u64) -> String {
    let f = n as f64;
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => trim(format!("{:.1}", f / 1e3)) + "K",
        1_000_000..=999_999_999 => trim(format!("{:.1}", f / 1e6)) + "M",
        _ => trim(format!("{:.2}", f / 1e9)) + "B",
    }
}

fn trim(s: String) -> String {
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s }
}

pub fn count(n: u64) -> String {
    group(n as i64)
}

pub fn ms(v: f64) -> String {
    if v < 1000.0 {
        format!("{v:.0} ms")
    } else if v < 60_000.0 {
        format!("{:.1} s", v / 1000.0)
    } else {
        format!("{:.1} min", v / 60_000.0)
    }
}

pub fn tps(v: f64) -> String {
    if v >= 100.0 { format!("{v:.0}") } else { format!("{v:.1}") }
}

pub fn percent(v: f64) -> String {
    if v >= 0.995 {
        "100%".into()
    } else if v < 0.001 && v > 0.0 {
        "<0.1%".into()
    } else {
        format!("{:.1}%", v * 100.0)
    }
}

/// "2h 14m", "45m", "30s"
pub fn span(ms: i64) -> String {
    let s = ms.max(0) / 1000;
    let (d, h, m) = (s / 86400, (s % 86400) / 3600, (s % 3600) / 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{s}s")
    }
}

pub fn clock(ms: i64) -> String {
    crate::analytics::local(ms).format("%H:%M").to_string()
}

pub fn ago(ms: i64, now: i64) -> String {
    let d = now - ms;
    if d < 60_000 {
        "just now".into()
    } else if d < 3_600_000 {
        format!("{} min ago", d / 60_000)
    } else if d < 86_400_000 {
        format!("{} h ago", d / 3_600_000)
    } else {
        format!("{} d ago", d / 86_400_000)
    }
}

pub fn day_title(d: NaiveDate) -> String {
    let today = Local::now().date_naive();
    match (today - d).num_days() {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        _ => d.format("%A").to_string(),
    }
}

pub fn date_long(d: NaiveDate) -> String {
    d.format("%B %-d, %Y").to_string()
}

pub fn date_short(d: NaiveDate) -> String {
    d.format("%b %-d").to_string()
}
