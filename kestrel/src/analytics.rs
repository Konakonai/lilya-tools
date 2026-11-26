//! Aggregations over a [`Dataset`] slice, shared by the CLI and the UI.

use std::collections::HashMap;

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, TimeZone};

use crate::ingest::Dataset;
use crate::model::{Agent, Call, Tokens, project_name};
use crate::pricing::Match;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Metric {
    #[default]
    Cost,
    Tokens,
    Requests,
}

impl Metric {
    pub fn of(self, c: &Call) -> f64 {
        match self {
            Metric::Cost => c.usd,
            Metric::Tokens => c.tokens.total() as f64,
            Metric::Requests => c.calls as f64,
        }
    }

    pub fn of_totals(self, t: &Totals) -> f64 {
        match self {
            Metric::Cost => t.usd,
            Metric::Tokens => t.tokens.total() as f64,
            Metric::Requests => t.requests as f64,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Totals {
    pub usd: f64,
    pub saved: f64,
    pub tokens: Tokens,
    pub requests: u64,
    pub unpriced: u64,
}

impl Totals {
    pub fn add(&mut self, c: &Call) {
        self.usd += c.usd;
        self.saved += c.saved_usd;
        self.tokens.add(&c.tokens);
        self.requests += c.calls as u64;
        if c.priced == Match::None {
            self.unpriced += c.calls as u64;
        }
    }

    pub fn merge(&mut self, o: &Totals) {
        self.usd += o.usd;
        self.saved += o.saved;
        self.tokens.add(&o.tokens);
        self.requests += o.requests;
        self.unpriced += o.unpriced;
    }

    /// Share of prompt tokens served from cache.
    pub fn cache_hit(&self) -> Option<f64> {
        let p = self.tokens.prompt();
        (p > 0).then(|| self.tokens.cache_read as f64 / p as f64)
    }
}

pub fn totals<'a>(calls: impl IntoIterator<Item = &'a Call>) -> Totals {
    let mut t = Totals::default();
    for c in calls {
        t.add(c);
    }
    t
}

/// Distribution summary of output speed and latency.
#[derive(Clone, Debug, Default)]
pub struct Speed {
    pub tps: Option<Quantiles>,
    pub ttft: Option<Quantiles>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Quantiles {
    pub n: usize,
    pub p10: f64,
    pub p25: f64,
    pub p50: f64,
    pub p75: f64,
    pub p90: f64,
}

impl Quantiles {
    pub fn of(mut v: Vec<f64>) -> Option<Quantiles> {
        if v.is_empty() {
            return None;
        }
        v.sort_by(|a, b| a.total_cmp(b));
        let q = |p: f64| {
            let idx = p * (v.len() - 1) as f64;
            let (lo, hi) = (idx.floor() as usize, idx.ceil() as usize);
            v[lo] + (v[hi] - v[lo]) * (idx - lo as f64)
        };
        Some(Quantiles { n: v.len(), p10: q(0.1), p25: q(0.25), p50: q(0.5), p75: q(0.75), p90: q(0.9) })
    }
}

pub fn speed<'a>(calls: impl IntoIterator<Item = &'a Call>) -> Speed {
    let mut tps = Vec::new();
    let mut ttft = Vec::new();
    for c in calls {
        if let Some(v) = c.tps() {
            tps.push(v);
        }
        if let Some(t) = c.ttft_ms {
            ttft.push(t as f64);
        }
    }
    Speed { tps: Quantiles::of(tps), ttft: Quantiles::of(ttft) }
}

/// Totals plus speed for one group (agent, model, project...).
#[derive(Clone, Debug)]
pub struct Group {
    pub key: String,
    pub agent: Agent,
    pub totals: Totals,
    pub speed: Speed,
    pub sessions: usize,
    pub last_ts: i64,
}

pub fn group_by<'a, F>(calls: &'a [Call], key: F) -> Vec<Group>
where
    F: Fn(&'a Call) -> String,
{
    struct Acc<'a> {
        agent_counts: HashMap<Agent, u64>,
        totals: Totals,
        calls: Vec<&'a Call>,
        sessions: std::collections::HashSet<(Agent, &'a str)>,
        last: i64,
    }
    let mut map: HashMap<String, Acc> = HashMap::new();
    for c in calls {
        let a = map.entry(key(c)).or_insert_with(|| Acc {
            agent_counts: HashMap::new(),
            totals: Totals::default(),
            calls: Vec::new(),
            sessions: Default::default(),
            last: 0,
        });
        *a.agent_counts.entry(c.agent).or_default() += 1;
        a.totals.add(c);
        a.calls.push(c);
        a.sessions.insert((c.agent, c.session.as_str()));
        a.last = a.last.max(c.ts);
    }
    let mut out: Vec<Group> = map
        .into_iter()
        .map(|(key, a)| {
            let agent = a.agent_counts.iter().max_by_key(|(ag, n)| (**n, std::cmp::Reverse(**ag))).map(|(ag, _)| *ag).unwrap_or_default();
            Group { key, agent, speed: speed(a.calls.iter().copied()), totals: a.totals, sessions: a.sessions.len(), last_ts: a.last }
        })
        .collect();
    out.sort_by(|a, b| b.totals.usd.total_cmp(&a.totals.usd).then(b.totals.requests.cmp(&a.totals.requests)));
    out
}

/// One session's footprint within a time slice.
#[derive(Clone, Debug)]
pub struct SessionRow {
    pub agent: Agent,
    pub id: String,
    pub title: String,
    pub project: Option<String>,
    pub start: i64,
    pub end: i64,
    pub totals: Totals,
    pub model: String,
    pub models: Vec<String>,
    /// Peak context size seen in the session.
    pub peak_context: u64,
    /// Subagent conversations folded into this session.
    pub subagents: usize,
    /// Indexes into the slice, in time order.
    pub calls: Vec<usize>,
}

/// The top-level session a subagent conversation belongs to (itself if none).
pub fn root_session<'a>(ds: &'a Dataset, agent: Agent, id: &'a str) -> &'a str {
    let mut cur = id;
    for _ in 0..8 {
        match ds.session(agent, cur).and_then(|m| m.parent.as_deref()) {
            Some(p) if p != cur => cur = p,
            _ => break,
        }
    }
    cur
}

/// Sessions in a slice of calls, with subagent conversations folded into
/// the session that spawned them.
pub fn sessions<'a>(ds: &'a Dataset, calls: &'a [Call]) -> Vec<SessionRow> {
    let mut map: HashMap<(Agent, &str), SessionRow> = HashMap::new();
    let mut model_use: HashMap<(Agent, &str), HashMap<&str, f64>> = HashMap::new();
    let mut subs: HashMap<(Agent, &str), std::collections::HashSet<&str>> = HashMap::new();
    for (i, c) in calls.iter().enumerate() {
        let root = root_session(ds, c.agent, &c.session);
        let k = (c.agent, root);
        if root != c.session {
            subs.entry(k).or_default().insert(c.session.as_str());
        }
        let row = map.entry(k).or_insert_with(|| {
            let meta = ds.session(c.agent, root);
            SessionRow {
                agent: c.agent,
                id: root.to_string(),
                title: meta.and_then(|m| m.title.clone()).unwrap_or_default(),
                project: meta.and_then(|m| m.cwd.as_deref()).map(project_name),
                start: c.ts,
                end: c.ts,
                totals: Totals::default(),
                model: String::new(),
                models: Vec::new(),
                peak_context: 0,
                subagents: 0,
                calls: Vec::new(),
            }
        });
        row.start = row.start.min(c.ts);
        row.end = row.end.max(c.ts + c.duration_ms.unwrap_or(0) as i64);
        row.totals.add(c);
        row.peak_context = row.peak_context.max(c.tokens.prompt());
        row.calls.push(i);
        *model_use.entry(k).or_default().entry(c.model.as_str()).or_default() += c.tokens.total() as f64 + 1.0;
    }
    let mut out: Vec<SessionRow> = map
        .into_iter()
        .map(|(k, mut r)| {
            r.subagents = subs.get(&k).map_or(0, |s| s.len());
            if let Some(m) = model_use.get(&k) {
                let mut ms: Vec<(&str, f64)> = m.iter().map(|(a, b)| (*a, *b)).collect();
                ms.sort_by(|a, b| b.1.total_cmp(&a.1));
                r.model = ms.first().map(|x| x.0.to_string()).unwrap_or_default();
                r.models = ms.into_iter().map(|x| x.0.to_string()).collect();
            }
            if r.title.is_empty() {
                r.title = r.project.clone().unwrap_or_else(|| short_id(&r.id));
            }
            r
        })
        .collect();
    out.sort_by_key(|r| r.start);
    out
}

pub fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

// ---------------------------------------------------------------------------
// Calendar helpers

pub fn local_midnight(d: NaiveDate) -> i64 {
    let naive = d.and_hms_opt(0, 0, 0).unwrap();
    match Local.from_local_datetime(&naive) {
        chrono::LocalResult::Single(t) => t.timestamp_millis(),
        chrono::LocalResult::Ambiguous(a, _) => a.timestamp_millis(),
        chrono::LocalResult::None => Local.from_utc_datetime(&naive).timestamp_millis(),
    }
}

pub fn day_bounds(d: NaiveDate) -> (i64, i64) {
    (local_midnight(d), local_midnight(d + Duration::days(1)))
}

pub fn today() -> NaiveDate {
    Local::now().date_naive()
}

pub fn local(ms: i64) -> DateTime<Local> {
    Local.timestamp_millis_opt(ms).single().unwrap_or_else(Local::now)
}

pub fn date_of(ms: i64) -> NaiveDate {
    local(ms).date_naive()
}

/// Per-day totals split by agent, for `days` days ending at `last`.
#[derive(Clone, Debug)]
pub struct DayRow {
    pub date: NaiveDate,
    pub by_agent: Vec<(Agent, Totals)>,
    pub totals: Totals,
}

pub fn daily(ds: &Dataset, first: NaiveDate, last: NaiveDate) -> Vec<DayRow> {
    let mut out = Vec::new();
    let mut d = first;
    while d <= last {
        let (a, b) = day_bounds(d);
        let mut per: HashMap<Agent, Totals> = HashMap::new();
        let mut t = Totals::default();
        for c in ds.between(a, b) {
            per.entry(c.agent).or_default().add(c);
            t.add(c);
        }
        let mut by_agent: Vec<(Agent, Totals)> = per.into_iter().collect();
        by_agent.sort_by_key(|(a, _)| Agent::ALL.iter().position(|x| x == a));
        out.push(DayRow { date: d, by_agent, totals: t });
        d += Duration::days(1);
    }
    out
}

/// Monday of the week containing `d`.
pub fn week_start(d: NaiveDate) -> NaiveDate {
    d - Duration::days(d.weekday().num_days_from_monday() as i64)
}

/// Consecutive active days ending today (or yesterday, if today is still empty).
pub fn streak(ds: &Dataset, today: NaiveDate) -> u32 {
    let mut n = 0;
    let mut d = today;
    let (a, b) = day_bounds(d);
    if ds.between(a, b).is_empty() {
        d -= Duration::days(1);
    }
    loop {
        let (a, b) = day_bounds(d);
        if ds.between(a, b).is_empty() {
            return n;
        }
        n += 1;
        d -= Duration::days(1);
        if n > 3650 {
            return n;
        }
    }
}
