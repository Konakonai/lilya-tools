//! Today: one day in detail. Headline numbers, rate limits, a timeline of
//! every session and the models that were used.

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use chrono::{Datelike, Duration, NaiveDate, Timelike};

use super::Ctx;
use super::charts::composition::Composition;
use super::charts::timeline::{self, Timeline};
use super::charts::{esc, meter};
use super::widgets::{self, Stat, Table};
use crate::analytics::{self, SessionRow};
use crate::format;
use crate::ingest::Dataset;
use crate::model::{Agent, Call, QuotaWindow};

pub struct TodayPage {
    pub root: gtk::ScrolledWindow,
    ctx: Weak<Ctx>,
    day: Cell<NaiveDate>,
    /// The date "today" had at the last refresh, to follow midnight.
    seen_today: Cell<NaiveDate>,
    title: gtk::Label,
    subtitle: gtk::Label,
    next: gtk::Button,
    today_btn: gtk::Button,
    calendar: gtk::Calendar,
    spend: Stat,
    tokens: Stat,
    requests: Stat,
    speed: Stat,
    composition: Composition,
    limits_card: gtk::Box,
    limits: gtk::Box,
    legend: gtk::Box,
    timeline: Timeline,
    models_card: gtk::Box,
    models_count: gtk::Label,
    models: RefCell<Table>,
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Collapses whitespace so multi-line titles fit on one line.
pub fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl TodayPage {
    pub fn new(ctx: &Rc<Ctx>) -> Rc<TodayPage> {
        let content = widgets::vbox(18);

        let header = widgets::hbox(12);
        let titles = widgets::vbox(0);
        let title = widgets::label("Today", &["day-title"]);
        let subtitle = widgets::label("", &["day-subtitle"]);
        titles.append(&title);
        titles.append(&subtitle);
        header.append(&titles);
        header.append(&widgets::spacer());
        let nav = widgets::hbox(0);
        nav.add_css_class("linked");
        nav.set_valign(gtk::Align::Center);
        let prev = gtk::Button::from_icon_name("go-previous-symbolic");
        prev.set_tooltip_text(Some("Previous day (Alt+←)"));
        let today_btn = gtk::Button::with_label("Today");
        today_btn.set_tooltip_text(Some("Back to today (Alt+Home)"));
        let next = gtk::Button::from_icon_name("go-next-symbolic");
        next.set_tooltip_text(Some("Next day (Alt+→)"));
        nav.append(&prev);
        nav.append(&today_btn);
        nav.append(&next);
        let calendar = gtk::Calendar::new();
        let pop = gtk::Popover::builder().child(&calendar).build();
        let pick = gtk::MenuButton::builder()
            .icon_name("x-office-calendar-symbolic")
            .popover(&pop)
            .tooltip_text("Go to a day")
            .valign(gtk::Align::Center)
            .build();
        header.append(&nav);
        header.append(&pick);
        content.append(&header);

        let hero = widgets::card();
        hero.add_css_class("hero");
        let spend = widgets::stat("Spend");
        let tokens = widgets::stat("Tokens");
        let requests = widgets::stat("Requests");
        let speed = widgets::stat("Output speed");
        let composition = Composition::new();
        tokens.extra.append(&composition.area);
        hero.append(&widgets::stat_row(&[&spend, &tokens, &requests, &speed]));
        content.append(&hero);

        let limits_card = widgets::card();
        let (h, _) = widgets::panel_header("Rate limits");
        limits_card.append(&h);
        let limits = widgets::vbox(12);
        limits_card.append(&limits);
        limits_card.set_visible(false);
        content.append(&limits_card);

        let activity = widgets::card();
        let (h, legend) = widgets::panel_header("Activity");
        activity.append(&h);
        let timeline = Timeline::new();
        activity.append(&timeline.area);
        content.append(&activity);

        let models_card = widgets::card();
        let (h, end) = widgets::panel_header("Models");
        let models_count = widgets::label("", &["dim"]);
        end.append(&models_count);
        models_card.append(&h);
        let table = Table::new(&[
            ("Model", 0.0),
            ("Requests", 1.0),
            ("Tokens", 1.0),
            ("Cache hit", 1.0),
            ("Speed", 1.0),
            ("First token", 1.0),
            ("Cost", 1.0),
        ]);
        models_card.append(&table.grid);
        content.append(&models_card);

        let today = analytics::today();
        let page = Rc::new(TodayPage {
            root: widgets::page(&content),
            ctx: Rc::downgrade(ctx),
            day: Cell::new(today),
            seen_today: Cell::new(today),
            title,
            subtitle,
            next: next.clone(),
            today_btn: today_btn.clone(),
            calendar: calendar.clone(),
            spend,
            tokens,
            requests,
            speed,
            composition,
            limits_card,
            limits,
            legend,
            timeline,
            models_card,
            models_count,
            models: RefCell::new(table),
        });

        let w = Rc::downgrade(&page);
        prev.connect_clicked(move |_| {
            if let Some(p) = w.upgrade() {
                p.step(-1);
            }
        });
        let w = Rc::downgrade(&page);
        next.connect_clicked(move |_| {
            if let Some(p) = w.upgrade() {
                p.step(1);
            }
        });
        let w = Rc::downgrade(&page);
        today_btn.connect_clicked(move |_| {
            if let Some(p) = w.upgrade() {
                p.set_day(analytics::today());
            }
        });
        let w = Rc::downgrade(&page);
        pop.connect_show(move |_| {
            if let Some(p) = w.upgrade() {
                let d = p.day.get();
                if let Ok(dt) = gtk::glib::DateTime::from_local(d.year(), d.month() as i32, d.day() as i32, 12, 0, 0.0) {
                    p.calendar.select_day(&dt);
                }
                p.mark_calendar();
            }
        });
        for prop in ["month", "year"] {
            let w = Rc::downgrade(&page);
            calendar.connect_notify_local(Some(prop), move |_, _| {
                if let Some(p) = w.upgrade() {
                    p.mark_calendar();
                }
            });
        }
        let (w, popw) = (Rc::downgrade(&page), pop.downgrade());
        calendar.connect_day_selected(move |c| {
            let dt = c.date();
            let Some(d) = NaiveDate::from_ymd_opt(dt.year(), dt.month() as u32, dt.day_of_month() as u32) else { return };
            if let Some(p) = w.upgrade()
                && d != p.day.get()
            {
                p.set_day(d);
                if let Some(pop) = popw.upgrade() {
                    pop.popdown();
                }
            }
        });
        let c = Rc::downgrade(ctx);
        *page.timeline.pick.borrow_mut() = Some(Box::new(move |agent, id| {
            if let Some(c) = c.upgrade() {
                c.open_session(agent, id);
            }
        }));
        page
    }

    pub fn day(&self) -> NaiveDate {
        self.day.get()
    }

    pub fn set_day(&self, d: NaiveDate) {
        self.day.set(d.min(analytics::today()));
        self.refresh();
    }

    pub fn step(&self, days: i64) {
        self.set_day(self.day.get() + Duration::days(days));
    }

    /// Marks days with activity in the month the calendar shows.
    fn mark_calendar(&self) {
        let Some(ctx) = self.ctx.upgrade() else { return };
        let ds = ctx.ds();
        let c = &self.calendar;
        c.clear_marks();
        let shown = c.date();
        let (y, m) = (shown.year(), shown.month() as u32);
        let Some(mut d) = NaiveDate::from_ymd_opt(y, m, 1) else { return };
        while d.month() == m {
            let (a, b) = analytics::day_bounds(d);
            if !ds.between(a, b).is_empty() {
                c.mark_day(d.day());
            }
            d += Duration::days(1);
        }
    }

    /// Called every 30 seconds: follows midnight and keeps "now" moving.
    pub fn tick(&self) {
        let today = analytics::today();
        if self.seen_today.get() != today && self.day.get() == self.seen_today.get() {
            self.day.set(today);
        }
        if self.day.get() == today {
            self.refresh();
        }
    }

    pub fn refresh(&self) {
        let Some(ctx) = self.ctx.upgrade() else { return };
        let ds = ctx.ds();
        let day = self.day.get();
        let today = analytics::today();
        self.seen_today.set(today);
        let is_today = day == today;
        let now = now_ms();
        let (a, b) = analytics::day_bounds(day);
        let calls = ds.between(a, b);
        let t = analytics::totals(calls);
        let sessions = analytics::sessions(&ds, calls);
        let agents: BTreeSet<Agent> = calls.iter().map(|c| c.agent).collect();

        self.title.set_label(&format::day_title(day));
        let mut sub = format::date_long(day);
        if let Some(last) = calls.last().filter(|_| is_today) {
            sub.push_str(&format!(" · last request {}", format::ago(last.ts, now)));
        }
        self.subtitle.set_label(&sub);
        self.next.set_sensitive(!is_today);
        self.today_btn.set_sensitive(!is_today);

        let mut spend = Vec::new();
        if t.saved >= 0.01 {
            spend.push(format!("{} saved by caching", format::usd(t.saved)));
        }
        if let Some(avg) = active_day_average(&ds, day, 30) {
            spend.push(format!("{} on a typical day", format::usd(avg)));
        }
        if t.unpriced > 0 {
            spend.push(format!("{} requests unpriced", format::count(t.unpriced)));
        }
        self.spend.set(&format::usd(t.usd), "", &spend.join("\n"));

        let tok = t.tokens;
        let mut tokens = Vec::new();
        if let Some(h) = t.cache_hit() {
            tokens.push(format!("{} cache hit", format::percent(h)));
        }
        if tok.output > 0 {
            tokens.push(format!("{} output", format::tokens(tok.output)));
        }
        self.tokens.set(&format::tokens(tok.total()), "", &tokens.join(" · "));
        self.composition.set(tok);

        let agent_text = match agents.len() {
            0 => String::new(),
            1..=3 => agents.iter().map(|a| a.label()).collect::<Vec<_>>().join(", "),
            n => format!("{n} agents"),
        };
        let mut req = vec![format!("{} {}", sessions.len(), if sessions.len() == 1 { "session" } else { "sessions" })];
        if !agent_text.is_empty() {
            req.push(agent_text);
        }
        self.requests.set(&format::count(t.requests), "", &req.join(" · "));

        let sp = analytics::speed(calls);
        let ttft = sp.ttft.map(|q| format!("first token in {} (median)", format::ms(q.p50))).unwrap_or_default();
        match sp.tps {
            Some(q) => self.speed.set(&format::tps(q.p50), "tok/s", &ttft),
            None => self.speed.set("—", "", if ttft.is_empty() { "No timing data" } else { &ttft }),
        }

        self.refresh_limits(&ds, is_today, now);

        widgets::clear(&self.legend);
        let mut per_agent: Vec<(Agent, f64)> =
            agents.iter().map(|a| (*a, calls.iter().filter(|c| c.agent == *a).map(|c| c.usd).sum())).collect();
        per_agent.sort_by(|x, y| y.1.total_cmp(&x.1));
        for (agent, usd) in per_agent {
            let chip = widgets::legend_chip(agent, &format!("{} {}", agent.label(), format::usd(usd)));
            self.legend.append(&chip);
        }
        self.timeline.set(timeline_data(calls, &sessions, day, now));

        let groups = analytics::group_by(calls, |c| format!("{}\u{1f}{}", c.agent.slug(), c.model));
        let mut table = self.models.borrow_mut();
        table.clear();
        for g in &groups {
            let (slug, model) = g.key.split_once('\u{1f}').unwrap_or(("", g.key.as_str()));
            let agent = Agent::from_slug(slug).unwrap_or(g.agent);
            let pill = (g.totals.unpriced > 0).then_some("unpriced");
            let cells = [
                format::count(g.totals.requests),
                format::tokens(g.totals.tokens.total()),
                g.totals.cache_hit().map(format::percent).unwrap_or_else(|| "—".into()),
                g.speed.tps.map(|q| format!("{} tok/s", format::tps(q.p50))).unwrap_or_else(|| "—".into()),
                g.speed.ttft.map(|q| format::ms(q.p50)).unwrap_or_else(|| "—".into()),
                format::usd_exact(g.totals.usd),
            ];
            table.push(&widgets::name_cell(agent, model, pill), &cells, &[]);
        }
        self.models_card.set_visible(!groups.is_empty());
        self.models_count.set_label(&match groups.len() {
            1 => "1 model".to_string(),
            n => format!("{n} models"),
        });
    }

    fn refresh_limits(&self, ds: &Dataset, is_today: bool, now: i64) {
        widgets::clear(&self.limits);
        let mut quotas: Vec<&QuotaWindow> = ds.quotas.iter().filter(|q| q.resets_at * 1000 > now).collect();
        quotas.sort_by_key(|q| (q.agent, q.window_minutes));
        self.limits_card.set_visible(is_today && !quotas.is_empty());
        for q in quotas {
            self.limits.append(&quota_row(q, now));
        }
    }
}

fn quota_row(q: &QuotaWindow, now: i64) -> gtk::Box {
    let row = widgets::hbox(16);
    let left = q.resets_at * 1000 - now;
    let elapsed = (q.window_minutes > 0).then(|| 1.0 - left as f64 / (q.window_minutes as f64 * 60_000.0));
    let used = q.used_percent / 100.0;

    let name = widgets::vbox(0);
    name.append(&widgets::label(&format!("{} · {}", q.agent.label(), q.label), &["session-title"]));
    if let Some(plan) = &q.plan {
        name.append(&widgets::label(&format!("{} plan", capitalize(plan)), &["stat-detail"]));
    }
    name.set_size_request(170, -1);
    row.append(&name);
    row.append(&meter::meter(used, elapsed));
    let pct = widgets::label(&format!("{:.0}%", q.used_percent), &["insight-value"]);
    pct.set_width_chars(5);
    pct.set_xalign(1.0);
    row.append(&pct);
    let reset = widgets::label(&format!("resets in {}", format::span(left)), &["dim", "numeric"]);
    reset.set_width_chars(17);
    reset.set_xalign(1.0);
    row.append(&reset);

    let mut tip = format!("{:.0}% of the {} window used", q.used_percent, q.label);
    if let Some(e) = elapsed {
        tip.push_str(&format!(", {:.0}% of the window has passed", e.clamp(0.0, 1.0) * 100.0));
        if used > e + 0.1 {
            tip.push_str(" — ahead of pace");
        }
    }
    tip.push_str(&format!(".\nResets at {}. Reported {}.", format::clock(q.resets_at * 1000), format::ago(q.observed_at, now)));
    row.set_tooltip_text(Some(&tip));
    row
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

/// Mean spend over active days among the `days` days before `day`.
fn active_day_average(ds: &Dataset, day: NaiveDate, days: i64) -> Option<f64> {
    let (mut total, mut active) = (0.0, 0);
    for k in 1..=days {
        let (a, b) = analytics::day_bounds(day - Duration::days(k));
        let calls = ds.between(a, b);
        if !calls.is_empty() {
            total += calls.iter().map(|c| c.usd).sum::<f64>();
            active += 1;
        }
    }
    (active >= 3).then(|| total / active as f64)
}

fn floor_hour(ms: i64) -> i64 {
    let l = analytics::local(ms);
    ms - (l.minute() as i64 * 60_000 + l.second() as i64 * 1000 + l.timestamp_subsec_millis() as i64)
}

fn ceil_hour(ms: i64) -> i64 {
    let f = floor_hour(ms);
    if f == ms { ms } else { f + 3_600_000 }
}

fn timeline_data(calls: &[Call], sessions: &[SessionRow], day: NaiveDate, now: i64) -> timeline::Data {
    let (a, b) = analytics::day_bounds(day);
    let is_today = (a..b).contains(&now);
    if sessions.is_empty() {
        let empty = if is_today { "No agent activity yet today" } else { "No agent activity on this day" };
        return timeline::Data { empty: empty.into(), ..Default::default() };
    }
    const HOUR: i64 = 3_600_000;
    let first = sessions.iter().map(|r| r.start).min().unwrap_or(a);
    let mut last = sessions.iter().map(|r| r.end).max().unwrap_or(b);
    if is_today {
        last = last.max(now);
    }
    let mut start = floor_hour(first - HOUR / 4).max(a);
    let mut end = ceil_hour(last + HOUR / 4).min(b);
    let min_span = 4 * HOUR;
    if end - start < min_span {
        let missing = min_span - (end - start);
        start = floor_hour(start - missing / 2).max(a);
        end = (start + min_span).min(b);
        start = (end - min_span).max(a);
    }
    let bucket_ms = if end - start > 12 * HOUR { 15 * 60_000 } else { 10 * 60_000 };
    let n = ((end - start + bucket_ms - 1) / bucket_ms) as usize;
    let mut buckets = vec![[0.0; 6]; n];
    let mut usd = vec![0.0; n];
    for c in calls {
        if c.ts < start {
            continue;
        }
        let i = ((c.ts - start) / bucket_ms) as usize;
        if i < n {
            let k = Agent::ALL.iter().position(|x| *x == c.agent).unwrap_or(0);
            buckets[i][k] += c.calls as f64;
            usd[i] += c.usd;
        }
    }
    let bucket_tips = (0..n)
        .map(|i| {
            let s = start + i as i64 * bucket_ms;
            let mut tip = format!("<b>{}–{}</b>", format::clock(s), format::clock(s + bucket_ms));
            for (k, v) in buckets[i].iter().enumerate() {
                if *v > 0.0 {
                    tip.push_str(&format!("\n{}  <b>{}</b> requests", Agent::ALL[k].label(), *v as u64));
                }
            }
            tip.push_str(&format!("\n{}", format::usd(usd[i])));
            tip
        })
        .collect();
    // Busy days can have hundreds of sessions: keep the costliest lanes and
    // fold the rest into one, so the chart stays a chart.
    const MAX_LANES: usize = 16;
    let mut shown: Vec<&SessionRow> = sessions.iter().collect();
    let mut rest = Vec::new();
    if shown.len() > MAX_LANES {
        shown.sort_by(|a, b| b.totals.usd.total_cmp(&a.totals.usd).then(b.totals.requests.cmp(&a.totals.requests)));
        rest = shown.split_off(MAX_LANES - 1);
        shown.sort_by_key(|r| r.start);
    }
    let mut lanes: Vec<timeline::Lane> = shown.into_iter().map(|r| lane(r, calls)).collect();
    if !rest.is_empty() {
        lanes.push(other_lane(&rest, calls));
    }
    timeline::Data { lanes, buckets, bucket_tips, bucket_ms, start, end, now: is_today.then_some(now), empty: String::new() }
}

fn lane(r: &SessionRow, calls: &[Call]) -> timeline::Lane {
    let mut title = one_line(&r.title);
    let project = r.project.as_ref().map(|p| format!(" · {}", esc(p))).unwrap_or_default();
    let subagents = match r.subagents {
        0 => String::new(),
        1 => " · 1 subagent".into(),
        n => format!(" · {n} subagents"),
    };
    let tip = format!(
        "<b>{}</b>\n{}{}{}\n{}–{} · {}\n{} requests · {} tokens · <b>{}</b>\n<small>{}</small>\n<small>Click for details</small>",
        esc(&title),
        r.agent.label(),
        project,
        subagents,
        format::clock(r.start),
        format::clock(r.end),
        format::span(r.end - r.start),
        format::count(r.totals.requests),
        format::tokens(r.totals.tokens.total()),
        format::usd(r.totals.usd),
        esc(&r.models.join(", ")),
    );
    if r.subagents > 0 {
        title.push_str(&format!(" +{}", r.subagents));
    }
    timeline::Lane {
        agent: r.agent,
        id: r.id.clone(),
        title,
        cost: format::usd(r.totals.usd),
        tip,
        start: r.start,
        end: r.end,
        ticks: r.calls.iter().map(|&i| calls[i].ts).collect(),
    }
}

/// One lane standing in for many small sessions.
fn other_lane(rest: &[&SessionRow], calls: &[Call]) -> timeline::Lane {
    let mut per_agent: std::collections::HashMap<Agent, u64> = Default::default();
    let mut t = analytics::Totals::default();
    let mut ticks = Vec::new();
    for r in rest {
        *per_agent.entry(r.agent).or_default() += r.totals.requests;
        t.merge(&r.totals);
        ticks.extend(r.calls.iter().map(|&i| calls[i].ts));
    }
    ticks.sort_unstable();
    let agent = per_agent.into_iter().max_by_key(|(_, n)| *n).map(|(a, _)| a).unwrap_or_default();
    let start = rest.iter().map(|r| r.start).min().unwrap_or(0);
    let end = rest.iter().map(|r| r.end).max().unwrap_or(start);
    let tip = format!(
        "<b>{} smaller sessions</b>\n{}–{}\n{} requests · {} tokens · <b>{}</b>\n<small>All of them are listed on the Sessions page</small>",
        rest.len(),
        format::clock(start),
        format::clock(end),
        format::count(t.requests),
        format::tokens(t.tokens.total()),
        format::usd(t.usd),
    );
    timeline::Lane {
        agent,
        id: String::new(),
        title: format!("{} more sessions", rest.len()),
        cost: format::usd(t.usd),
        tip,
        start,
        end,
        ticks,
    }
}
