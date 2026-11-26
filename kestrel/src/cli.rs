//! Terminal reports and the JSON status used by bar widgets.

use chrono::Duration;
use serde_json::json;

use crate::analytics::{self, Group};
use crate::format;
use crate::ingest::Dataset;
use crate::model::{Agent, project_name};

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum Range {
    Today,
    Yesterday,
    #[value(name = "7d")]
    Week,
    #[value(name = "30d")]
    Month,
    #[value(name = "90d")]
    Quarter,
    All,
}

impl Range {
    pub fn bounds(self) -> (i64, i64, &'static str) {
        let today = analytics::today();
        let end = analytics::day_bounds(today).1;
        let start = |days: i64| analytics::local_midnight(today - Duration::days(days - 1));
        match self {
            Range::Today => (start(1), end, "today"),
            Range::Yesterday => {
                let (a, b) = analytics::day_bounds(today - Duration::days(1));
                (a, b, "yesterday")
            }
            Range::Week => (start(7), end, "last 7 days"),
            Range::Month => (start(30), end, "last 30 days"),
            Range::Quarter => (start(90), end, "last 90 days"),
            Range::All => (i64::MIN / 2, end, "all time"),
        }
    }
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum By {
    Agent,
    Model,
    Project,
    Session,
}

pub fn report(ds: &Dataset, range: Range, by: By, json_out: bool) {
    let (a, b, label) = range.bounds();
    let calls = ds.between(a, b);
    let total = analytics::totals(calls);
    let groups: Vec<Group> = match by {
        By::Agent => analytics::group_by(calls, |c| c.agent.label().to_string()),
        By::Model => analytics::group_by(calls, |c| c.model.clone()),
        By::Project => analytics::group_by(calls, |c| {
            ds.session(c.agent, &c.session).and_then(|m| m.cwd.as_deref()).map(project_name).unwrap_or_else(|| "(unknown)".into())
        }),
        By::Session => analytics::group_by(calls, |c| {
            ds.session(c.agent, &c.session).and_then(|m| m.title.clone()).unwrap_or_else(|| analytics::short_id(&c.session))
        }),
    };

    if json_out {
        let rows: Vec<_> = groups
            .iter()
            .map(|g| {
                json!({
                    "key": g.key, "agent": g.agent, "usd": round(g.totals.usd), "requests": g.totals.requests,
                    "tokens": g.totals.tokens, "cache_hit": g.totals.cache_hit(),
                    "tps_p50": g.speed.tps.map(|q| q.p50), "ttft_p50_ms": g.speed.ttft.map(|q| q.p50),
                })
            })
            .collect();
        let out = json!({
            "range": label, "usd": round(total.usd), "saved_usd": round(total.saved), "requests": total.requests,
            "tokens": total.tokens, "unpriced_requests": total.unpriced, "groups": rows,
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
        return;
    }

    let t = &total.tokens;
    println!("\x1b[1mKestrel\x1b[0m · {label} · {} requests · {} tokens\n", format::count(total.requests), format::tokens(t.total()));
    println!("  Cost   \x1b[1m{}\x1b[0m   (cache saved {})", format::usd_exact(total.usd), format::usd(total.saved));
    println!(
        "  Tokens input {} · output {} · cache read {} · cache write {}{}",
        format::tokens(t.input),
        format::tokens(t.output),
        format::tokens(t.cache_read),
        format::tokens(t.cache_write + t.cache_write_1h),
        total.cache_hit().map(|h| format!(" · hit {}", format::percent(h))).unwrap_or_default()
    );
    let sp = analytics::speed(calls);
    if let (Some(tps), Some(ttft)) = (sp.tps, sp.ttft) {
        println!("  Speed  {} tok/s median · first token {} median\n", format::tps(tps.p50), format::ms(ttft.p50));
    } else {
        println!();
    }
    let width = groups.iter().map(|g| g.key.chars().count()).max().unwrap_or(10).clamp(10, 44);
    println!(
        "  \x1b[2m{:<width$}  {:>11}  {:>9}  {:>9}  {:>7}  {:>8}  {:>8}\x1b[0m",
        by_label(by),
        "cost",
        "requests",
        "tokens",
        "hit",
        "tok/s",
        "ttft"
    );
    for g in groups.iter().take(40) {
        let name: String = g.key.chars().take(width).collect();
        println!(
            "  {:<width$}  {:>11}  {:>9}  {:>9}  {:>7}  {:>8}  {:>8}",
            name,
            format::usd_exact(g.totals.usd),
            format::count(g.totals.requests),
            format::tokens(g.totals.tokens.total()),
            g.totals.cache_hit().map(format::percent).unwrap_or_default(),
            g.speed.tps.map(|q| format::tps(q.p50)).unwrap_or_else(|| "—".into()),
            g.speed.ttft.map(|q| format::ms(q.p50)).unwrap_or_else(|| "—".into()),
        );
    }
    let unpriced: Vec<String> =
        analytics::group_by(calls, |c| c.model.clone()).into_iter().filter(|g| g.totals.unpriced > 0).map(|g| g.key).collect();
    if !unpriced.is_empty() {
        println!("\n  \x1b[33m{} requests have no price:\x1b[0m {}", total.unpriced, unpriced.join(", "));
    }
    let now = chrono::Utc::now().timestamp();
    let live: Vec<_> = ds.quotas.iter().filter(|q| q.resets_at > now).collect();
    if !live.is_empty() {
        println!();
    }
    for q in live {
        println!(
            "  {} {} window {:.0}% used, resets in {}",
            q.agent.label(),
            q.label,
            q.used_percent,
            format::span((q.resets_at - now) * 1000)
        );
    }
}

fn by_label(by: By) -> &'static str {
    match by {
        By::Agent => "agent",
        By::Model => "model",
        By::Project => "project",
        By::Session => "session",
    }
}

fn round(v: f64) -> f64 {
    (v * 10000.0).round() / 10000.0
}

/// Compact JSON for status bars: today, live activity and quotas.
pub fn status(ds: &Dataset) {
    let now = chrono::Utc::now().timestamp_millis();
    let (a, b) = analytics::day_bounds(analytics::today());
    let today = ds.between(a, b);
    let t = analytics::totals(today);
    let recent = ds.between(now - 5 * 60_000, now + 60_000);
    let mut live: Vec<(Agent, String)> = recent.iter().map(|c| (c.agent, c.session.clone())).collect();
    live.sort();
    live.dedup();
    let sp = analytics::speed(recent);
    let quotas: Vec<_> = ds
        .quotas
        .iter()
        .filter(|q| q.resets_at * 1000 > now)
        .map(|q| json!({"agent": q.agent, "window": q.label, "used_percent": q.used_percent, "resets_at": q.resets_at}))
        .collect();
    let out = json!({
        "today": {"usd": round(t.usd), "requests": t.requests, "tokens": t.tokens.total(), "cache_hit": t.cache_hit()},
        "live": {"sessions": live.len(), "agents": live.iter().map(|x| x.0).collect::<std::collections::BTreeSet<_>>(),
                 "tps_p50": sp.tps.map(|q| q.p50), "ttft_p50_ms": sp.ttft.map(|q| q.p50)},
        "quotas": quotas,
        "text": format!("{} · {}", format::usd(t.usd), format::tokens(t.tokens.total())),
    });
    println!("{out}");
}
