//! History: trends over days and weeks, a year heatmap, and what stood out.

use std::collections::HashMap;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use chrono::{Datelike, Duration, NaiveDate, Timelike};

use super::Ctx;
use super::charts::bars::{self, Bars};
use super::charts::heatmap::{self, Heatmap};
use super::charts::ranked::{self, Ranked};
use super::today::one_line;
use super::widgets::{self, Stat};
use crate::analytics::{self, DayRow, Metric, Totals};
use crate::format;
use crate::model::{Agent, project_name};

pub struct HistoryPage {
    pub root: gtk::ScrolledWindow,
    ctx: Weak<Ctx>,
    range: adw::ToggleGroup,
    metric: adw::ToggleGroup,
    total: Stat,
    average: Stat,
    active: Stat,
    saved: Stat,
    chart_title: gtk::Label,
    legend: gtk::Box,
    bars: Bars,
    heat_note: gtk::Label,
    heatmap: Heatmap,
    insights: gtk::Box,
    projects_title: gtk::Label,
    projects: Ranked,
    models_title: gtk::Label,
    models: Ranked,
}

fn fmt_for(m: Metric) -> fn(f64) -> String {
    match m {
        Metric::Cost => format::usd,
        Metric::Tokens => |v| format::tokens(v as u64),
        Metric::Requests => |v| format::count(v as u64),
    }
}

fn noun(m: Metric) -> &'static str {
    match m {
        Metric::Cost => "spend",
        Metric::Tokens => "tokens",
        Metric::Requests => "requests",
    }
}

impl HistoryPage {
    pub fn new(ctx: &Rc<Ctx>) -> Rc<HistoryPage> {
        let content = widgets::vbox(18);

        let header = widgets::hbox(12);
        header.append(&widgets::label("History", &["day-title"]));
        header.append(&widgets::spacer());
        let metric = widgets::toggles(&[("cost", "Cost"), ("tokens", "Tokens"), ("requests", "Requests")], "cost");
        let range = widgets::toggles(&[("7d", "7 days"), ("30d", "30 days"), ("90d", "90 days"), ("1y", "Year")], "30d");
        header.append(&metric);
        header.append(&range);
        content.append(&header);

        let summary = widgets::card();
        summary.add_css_class("hero");
        let total = widgets::stat("Total");
        let average = widgets::stat("Per day");
        let active = widgets::stat("Active days");
        let saved = widgets::stat("Saved by caching");
        summary.append(&widgets::stat_row(&[&total, &average, &active, &saved]));
        content.append(&summary);

        let chart = widgets::card();
        let (h, legend) = widgets::panel_header("Daily spend");
        let chart_title = h.first_child().and_downcast::<gtk::Label>().expect("title label");
        chart.append(&h);
        let bars = Bars::new(250);
        chart.append(&bars.area);
        content.append(&chart);

        let insights = widgets::hbox(12);
        insights.set_homogeneous(true);
        content.append(&insights);

        let heat = widgets::card();
        let (h, end) = widgets::panel_header("Past year");
        let heat_note = widgets::label("", &["dim", "numeric"]);
        end.append(&heat_note);
        heat.append(&h);
        let heatmap = Heatmap::new();
        heat.append(&heatmap.area);
        content.append(&heat);

        let lists = widgets::hbox(18);
        lists.set_homogeneous(true);
        let pcard = widgets::card();
        let (h, _) = widgets::panel_header("Projects");
        let projects_title = h.first_child().and_downcast::<gtk::Label>().expect("title label");
        pcard.append(&h);
        let projects = Ranked::new();
        pcard.append(&projects.area);
        let mcard = widgets::card();
        let (h, _) = widgets::panel_header("Models");
        let models_title = h.first_child().and_downcast::<gtk::Label>().expect("title label");
        mcard.append(&h);
        let models = Ranked::new();
        mcard.append(&models.area);
        lists.append(&pcard);
        lists.append(&mcard);
        content.append(&lists);

        let page = Rc::new(HistoryPage {
            root: widgets::page(&content),
            ctx: Rc::downgrade(ctx),
            range: range.clone(),
            metric: metric.clone(),
            total,
            average,
            active,
            saved,
            chart_title,
            legend,
            bars,
            heat_note,
            heatmap,
            insights,
            projects_title,
            projects,
            models_title,
            models,
        });
        for g in [&range, &metric] {
            let w = Rc::downgrade(&page);
            g.connect_active_name_notify(move |_| {
                if let Some(p) = w.upgrade() {
                    p.refresh();
                }
            });
        }
        let c = Rc::downgrade(ctx);
        *page.bars.pick.borrow_mut() = Some(Box::new(move |d| {
            if let Some(c) = c.upgrade() {
                c.show_day(d);
            }
        }));
        let c = Rc::downgrade(ctx);
        *page.heatmap.pick.borrow_mut() = Some(Box::new(move |d| {
            if let Some(c) = c.upgrade() {
                c.show_day(d);
            }
        }));
        page
    }

    fn metric(&self) -> Metric {
        match widgets::active(&self.metric).as_str() {
            "tokens" => Metric::Tokens,
            "requests" => Metric::Requests,
            _ => Metric::Cost,
        }
    }

    pub fn refresh(&self) {
        let Some(ctx) = self.ctx.upgrade() else { return };
        let ds = ctx.ds();
        let metric = self.metric();
        let fmt = fmt_for(metric);
        let today = analytics::today();
        let range = widgets::active(&self.range);
        let weekly = range == "1y";
        let first = match range.as_str() {
            "7d" => today - Duration::days(6),
            "90d" => today - Duration::days(89),
            "1y" => analytics::week_start(today) - Duration::weeks(51),
            _ => today - Duration::days(29),
        };
        let days = analytics::daily(&ds, first, today);
        let (a, _) = analytics::day_bounds(first);
        let (_, b) = analytics::day_bounds(today);
        let calls = ds.between(a, b);
        let t = analytics::totals(calls);
        let n_days = days.len().max(1) as f64;
        let active_days: Vec<&DayRow> = days.iter().filter(|d| d.totals.requests > 0).collect();

        // Summary.
        let value = metric.of_totals(&t);
        let others = match metric {
            Metric::Cost => format!("{} tokens · {} requests", format::tokens(t.tokens.total()), format::count(t.requests)),
            Metric::Tokens => format!("{} · {} requests", format::usd(t.usd), format::count(t.requests)),
            Metric::Requests => format!("{} · {} tokens", format::usd(t.usd), format::tokens(t.tokens.total())),
        };
        self.total.set(&fmt(value), "", &others);
        let per_active = if active_days.is_empty() { 0.0 } else { value / active_days.len() as f64 };
        let per_active_text = if active_days.len() < days.len() && !active_days.is_empty() {
            format!("{} per active day", fmt(per_active))
        } else {
            String::new()
        };
        self.average.set(&fmt(value / n_days), "", &per_active_text);
        let streak = analytics::streak(&ds, today);
        let best = best_streak(&ds).max(streak);
        let runs = if streak > 1 && streak == best {
            format!("On a {streak}-day streak, your longest")
        } else if streak > 1 {
            format!("On a {streak}-day streak · best {best}")
        } else {
            format!("Longest streak {}", plural(best, "day"))
        };
        self.active.set(&format!("{}", active_days.len()), &format!("of {}", days.len()), &runs);
        self.saved.set(
            &format::usd(t.saved),
            "",
            &t.cache_hit().map(|h| format!("{} of prompt tokens read from cache", format::percent(h))).unwrap_or_default(),
        );

        // Bars.
        self.chart_title.set_label(&format!("{} {}", if weekly { "Weekly" } else { "Daily" }, noun(metric)));
        let (bars, labels) = if weekly { weekly_bars(&days, metric) } else { daily_bars(&days, metric) };
        let shown = ctx.window().map(|w| w.today.day());
        let mark = if weekly { None } else { shown.and_then(|d| days.iter().position(|r| r.date == d)) };
        self.bars.set(bars::Data { bars, labels, fmt, mark });
        widgets::clear(&self.legend);
        let mut per_agent: HashMap<Agent, f64> = HashMap::new();
        for c in calls {
            *per_agent.entry(c.agent).or_default() += metric.of(c);
        }
        for agent in Agent::ALL {
            if let Some(v) = per_agent.get(&agent).filter(|v| **v > 0.0) {
                self.legend.append(&widgets::legend_chip(agent, &format!("{} {}", agent.label(), fmt(*v))));
            }
        }

        // Insights.
        widgets::clear(&self.insights);
        for w in insights(&ctx, calls, &days, metric) {
            self.insights.append(&w);
        }

        // Heatmap: always the past year.
        let hfirst = analytics::week_start(today) - Duration::weeks(52);
        let year = analytics::daily(&ds, hfirst, today);
        let mut vals: Vec<f64> = year.iter().map(|d| metric.of_totals(&d.totals)).filter(|v| *v > 0.0).collect();
        vals.sort_by(|a, b| a.total_cmp(b));
        let q = |p: f64| vals.get(((vals.len() as f64 - 1.0) * p).round().max(0.0) as usize).copied().unwrap_or(0.0);
        let levels = [q(0.25), q(0.5), q(0.75)];
        let year_total: f64 = year.iter().map(|d| metric.of_totals(&d.totals)).sum();
        let year_active = year.iter().filter(|d| d.totals.requests > 0).count();
        self.heat_note.set_label(&format!("{} · {} active days", fmt(year_total), year_active));
        let days_out = year
            .iter()
            .map(|d| heatmap::Day { date: d.date, value: metric.of_totals(&d.totals), tip: day_tip(d.date, &d.totals) })
            .collect();
        self.heatmap.set(heatmap::Data { days: days_out, levels, today: Some(today), mark: shown });

        // Projects and models.
        let groups = analytics::group_by(calls, |c| {
            ds.session(c.agent, &c.session).and_then(|m| m.cwd.as_deref()).map(project_name).unwrap_or_else(|| "No project".into())
        });
        self.projects_title.set_label(&format!("Projects by {}", noun(metric)));
        self.projects.set(ranked_items(groups, metric, fmt, 8));
        let groups = analytics::group_by(calls, |c| c.model.clone());
        self.models_title.set_label(&format!("Models by {}", noun(metric)));
        self.models.set(ranked_items(groups, metric, fmt, 8));
    }
}

fn plural(n: u32, word: &str) -> String {
    if n == 1 { format!("1 {word}") } else { format!("{n} {word}s") }
}

/// Longest run of consecutive active days in the whole history.
fn best_streak(ds: &crate::ingest::Dataset) -> u32 {
    let days: std::collections::BTreeSet<NaiveDate> = ds.calls.iter().map(|c| analytics::date_of(c.ts)).collect();
    let (mut best, mut cur, mut prev) = (0, 0, None::<NaiveDate>);
    for d in days {
        cur = if prev.is_some_and(|p| d - p == Duration::days(1)) { cur + 1 } else { 1 };
        best = best.max(cur);
        prev = Some(d);
    }
    best
}

fn day_tip(d: NaiveDate, t: &Totals) -> String {
    let head = format!("<b>{}</b>", d.format("%a, %b %-d %Y"));
    if t.requests == 0 {
        return format!("{head}\nNo activity");
    }
    format!("{head}\n{} · {} tokens · {} requests", format::usd(t.usd), format::tokens(t.tokens.total()), format::count(t.requests))
}

fn segments(t: &[(Agent, Totals)], metric: Metric) -> Vec<(Agent, f64)> {
    t.iter().map(|(a, t)| (*a, metric.of_totals(t))).filter(|(_, v)| *v > 0.0).collect()
}

fn daily_bars(days: &[DayRow], metric: Metric) -> (Vec<bars::Bar>, Vec<(usize, String)>) {
    let n = days.len();
    let bars = days
        .iter()
        .map(|d| bars::Bar {
            title: d.date.format("%a, %b %-d").to_string(),
            day: Some(d.date),
            segments: segments(&d.by_agent, metric),
            total: metric.of_totals(&d.totals),
            note: if d.totals.requests > 0 { "<small>Click to open the day</small>".into() } else { String::new() },
        })
        .collect();
    let labels = days
        .iter()
        .enumerate()
        .filter(|(i, d)| match n {
            0..=10 => true,
            11..=40 => d.date.weekday() == chrono::Weekday::Mon || *i == 0,
            _ => d.date.day() == 1 || (d.date.weekday() == chrono::Weekday::Mon && d.date.iso_week().week() % 2 == 0),
        })
        .map(|(i, d)| (i, if n <= 10 { d.date.format("%a %-d").to_string() } else { format::date_short(d.date) }))
        .collect();
    (bars, labels)
}

fn weekly_bars(days: &[DayRow], metric: Metric) -> (Vec<bars::Bar>, Vec<(usize, String)>) {
    let mut bars = Vec::new();
    let mut labels = Vec::new();
    let mut prev_month = None;
    for (i, week) in days.chunks(7).enumerate() {
        let mut per: HashMap<Agent, Totals> = HashMap::new();
        let mut total = Totals::default();
        for d in week {
            for (a, t) in &d.by_agent {
                per.entry(*a).or_default().merge(t);
            }
            total.merge(&d.totals);
        }
        let mut by_agent: Vec<(Agent, Totals)> = per.into_iter().collect();
        by_agent.sort_by_key(|(a, _)| Agent::ALL.iter().position(|x| x == a));
        let start = week[0].date;
        let end = week[week.len() - 1].date;
        bars.push(bars::Bar {
            title: format!("{} – {}", format::date_short(start), format::date_short(end)),
            day: None,
            segments: segments(&by_agent, metric),
            total: metric.of_totals(&total),
            note: format!("<small>{} active days</small>", week.iter().filter(|d| d.totals.requests > 0).count()),
        });
        if prev_month != Some(start.month()) {
            if prev_month.is_some() {
                labels.push((i, start.format("%b").to_string()));
            }
            prev_month = Some(start.month());
        }
    }
    (bars, labels)
}

fn ranked_items(groups: Vec<analytics::Group>, metric: Metric, fmt: fn(f64) -> String, n: usize) -> Vec<ranked::Item> {
    let mut groups: Vec<_> = groups.into_iter().filter(|g| metric.of_totals(&g.totals) > 0.0).collect();
    groups.sort_by(|a, b| metric.of_totals(&b.totals).total_cmp(&metric.of_totals(&a.totals)));
    groups
        .into_iter()
        .take(n)
        .map(|g| {
            let v = metric.of_totals(&g.totals);
            ranked::Item {
                detail: format!(
                    "{} · {} tokens · {} requests\n{} sessions · mostly {}",
                    format::usd(g.totals.usd),
                    format::tokens(g.totals.tokens.total()),
                    format::count(g.totals.requests),
                    g.sessions,
                    g.agent.label()
                ),
                label: g.key,
                value: v,
                text: fmt(v),
                agent: g.agent,
            }
        })
        .collect()
}

fn insights(ctx: &Rc<Ctx>, calls: &[crate::model::Call], days: &[DayRow], metric: Metric) -> Vec<gtk::Widget> {
    let fmt = fmt_for(metric);
    let mut out = Vec::new();

    let busiest = days.iter().max_by(|a, b| metric.of_totals(&a.totals).total_cmp(&metric.of_totals(&b.totals)));
    match busiest.filter(|d| d.totals.requests > 0) {
        Some(d) => {
            let (c, date) = (Rc::downgrade(ctx), d.date);
            out.push(widgets::insight(
                "Busiest day",
                &d.date.format("%a, %b %-d").to_string(),
                &format!("{} · {} requests", fmt(metric.of_totals(&d.totals)), format::count(d.totals.requests)),
                Some(Box::new(move || {
                    if let Some(c) = c.upgrade() {
                        c.show_day(date);
                    }
                })),
            ));
        }
        None => out.push(widgets::insight("Busiest day", "—", "No activity yet", None)),
    }

    let mut hours = [0u64; 24];
    for c in calls {
        hours[analytics::local(c.ts).hour() as usize] += c.calls as u64;
    }
    let total: u64 = hours.iter().sum();
    if let Some((h, n)) = hours.iter().enumerate().max_by_key(|(_, n)| **n).filter(|(_, n)| **n > 0) {
        out.push(widgets::insight(
            "Peak hour",
            &format!("{h:02}:00 – {:02}:00", (h + 1) % 24),
            &format!("{} of requests start in this hour", format::percent(*n as f64 / total as f64)),
            None,
        ));
    }

    let ds = ctx.ds();
    let sessions = analytics::sessions(&ds, calls);
    if let Some(s) = sessions.iter().max_by(|a, b| a.totals.usd.total_cmp(&b.totals.usd)).filter(|s| s.totals.usd > 0.0) {
        let (c, agent, id) = (Rc::downgrade(ctx), s.agent, s.id.clone());
        out.push(widgets::insight(
            "Priciest session",
            &one_line(&s.title),
            &format!("{} · {} · {}", format::usd(s.totals.usd), s.agent.label(), format::date_short(analytics::date_of(s.start))),
            Some(Box::new(move || {
                if let Some(c) = c.upgrade() {
                    c.open_session(agent, &id);
                }
            })),
        ));
    }

    let groups = analytics::group_by(calls, |c| c.model.clone());
    if let Some((g, q)) =
        groups.iter().filter_map(|g| g.speed.tps.filter(|q| q.n >= 20).map(|q| (g, q))).max_by(|a, b| a.1.p50.total_cmp(&b.1.p50))
    {
        out.push(widgets::insight(
            "Fastest model",
            &g.key,
            &format!("{} tok/s median over {} requests", format::tps(q.p50), format::count(q.n as u64)),
            None,
        ));
    }
    out
}
