//! Models: cost and speed side by side, with speed and latency shown as
//! distributions on shared axes rather than single averages.

use std::rc::{Rc, Weak};

use adw::prelude::*;
use chrono::Duration;

use super::Ctx;
use super::charts::strip::{self, Scale};
use super::widgets::{self, Table};
use crate::analytics;
use crate::format;
use crate::model::Agent;

pub struct ModelsPage {
    pub root: gtk::ScrolledWindow,
    ctx: Weak<Ctx>,
    range: adw::ToggleGroup,
    summary: gtk::Label,
    holder: gtk::Box,
}

fn tps_label(v: f64) -> String {
    format!("{v:.0}")
}

fn secs_label(ms: f64) -> String {
    if ms < 1000.0 { format!("{:.1}s", ms / 1000.0) } else { format!("{:.0}s", ms / 1000.0) }
}

fn tps_full(v: f64) -> String {
    format!("{} tok/s", format::tps(v))
}

fn ms_full(v: f64) -> String {
    format::ms(v)
}

impl ModelsPage {
    pub fn new(ctx: &Rc<Ctx>) -> Rc<ModelsPage> {
        let content = widgets::vbox(18);
        let header = widgets::hbox(12);
        let titles = widgets::vbox(0);
        titles.append(&widgets::label("Models", &["day-title"]));
        let summary = widgets::label("", &["day-subtitle"]);
        titles.append(&summary);
        header.append(&titles);
        header.append(&widgets::spacer());
        let range = widgets::toggles(&[("1d", "Today"), ("7d", "7 days"), ("30d", "30 days"), ("90d", "90 days"), ("all", "All")], "30d");
        header.append(&range);
        content.append(&header);

        let card = widgets::card();
        let holder = widgets::vbox(0);
        card.append(&holder);
        content.append(&card);

        let note = widgets::label(
            "Speed is output tokens per second after the first token; first token is the wait before output starts. \
             The thin bar spans the 10th to 90th percentile, the thick bar the middle half, the tick is the median.",
            &["stat-detail"],
        );
        note.set_wrap(true);
        note.set_margin_start(4);
        content.append(&note);

        let page = Rc::new(ModelsPage { root: widgets::page(&content), ctx: Rc::downgrade(ctx), range: range.clone(), summary, holder });
        let w = Rc::downgrade(&page);
        range.connect_active_name_notify(move |_| {
            if let Some(p) = w.upgrade() {
                p.refresh();
            }
        });
        page
    }

    pub fn refresh(&self) {
        let Some(ctx) = self.ctx.upgrade() else { return };
        let ds = ctx.ds();
        let today = analytics::today();
        let (_, end) = analytics::day_bounds(today);
        let start = match widgets::active(&self.range).as_str() {
            "1d" => analytics::day_bounds(today).0,
            "7d" => analytics::day_bounds(today - Duration::days(6)).0,
            "90d" => analytics::day_bounds(today - Duration::days(89)).0,
            "all" => i64::MIN,
            _ => analytics::day_bounds(today - Duration::days(29)).0,
        };
        let calls = ds.between(start, end);
        let groups = analytics::group_by(calls, |c| format!("{}\u{1f}{}", c.agent.slug(), c.model));
        let t = analytics::totals(calls);
        self.summary.set_label(&format!("{} models · {} requests · {}", groups.len(), format::count(t.requests), format::usd(t.usd)));

        // Shared axes. Speed ends a little past the fastest typical model so one
        // outlier's tail does not squash every other strip to the left edge.
        let measured: Vec<_> = groups.iter().filter_map(|g| g.speed.tps).collect();
        let typical = measured.iter().map(|q| q.p75).fold(0.0, f64::max) * 1.3;
        let tps_max = measured.iter().map(|q| q.p90).fold(0.0, f64::max).min(typical).max(10.0);
        let (tps_top, tps_step) = crate::ui::theme::nice_scale(tps_max, 4);
        let tps_scale = Scale::Linear(tps_top);
        let tps_ticks: Vec<f64> = (0..=((tps_top / tps_step).round() as usize)).map(|i| i as f64 * tps_step).collect();
        let ttfts: Vec<_> = groups.iter().filter_map(|g| g.speed.ttft).collect();
        let lo = ttfts.iter().map(|q| q.p10).fold(f64::MAX, f64::min).max(100.0);
        let hi = ttfts.iter().map(|q| q.p90).fold(0.0, f64::max);
        let (lo, hi) = if hi > 0.0 {
            let lo = 10f64.powf(lo.log10().floor());
            (lo, 10f64.powf(hi.log10().ceil()).max(lo * 100.0))
        } else {
            (100.0, 100_000.0)
        };
        let ttft_scale = Scale::Log(lo, hi);
        let decades = (hi / lo).log10().round() as i32;
        let ttft_ticks: Vec<f64> = (0..=decades)
            .flat_map(|k| {
                let v = lo * 10f64.powi(k);
                if decades <= 2 && k < decades { vec![v, v * 3.0] } else { vec![v] }
            })
            .collect();

        let head = |text: &str, align: f32| {
            let l = widgets::label(text, &["table-head"]);
            l.set_xalign(align);
            l.upcast::<gtk::Widget>()
        };
        let axis_head = |text: &str, axis: gtk::DrawingArea| {
            let b = widgets::vbox(3);
            b.append(&widgets::label(text, &["table-head"]));
            b.append(&axis);
            b.upcast::<gtk::Widget>()
        };
        let mut table = Table::with_headers(vec![
            (head("Model", 0.0), 0.0),
            (head("Requests", 1.0), 1.0),
            (head("Tokens", 1.0), 1.0),
            (head("Cost", 1.0), 1.0),
            (head("Per request", 1.0), 1.0),
            (head("Cache hit", 1.0), 1.0),
            (axis_head("Output speed, tok/s", strip::axis(tps_scale, tps_ticks, tps_label)), 0.5),
            (head("Median", 1.0), 1.0),
            (axis_head("First token", strip::axis(ttft_scale, ttft_ticks, secs_label)), 0.5),
            (head("Median", 1.0), 1.0),
        ]);
        for g in &groups {
            let (slug, model) = g.key.split_once('\u{1f}').unwrap_or(("", g.key.as_str()));
            let agent = Agent::from_slug(slug).unwrap_or(g.agent);
            let pill = (g.totals.unpriced > 0).then_some("unpriced");
            let per_req = if g.totals.requests > 0 { g.totals.usd / g.totals.requests as f64 } else { 0.0 };
            let num = |s: String| {
                let l = widgets::label(&s, &["numeric"]);
                l.set_xalign(1.0);
                l.upcast::<gtk::Widget>()
            };
            let dim = |s: String| {
                let l = widgets::label(&s, &["numeric", "dim"]);
                l.set_xalign(1.0);
                l.upcast::<gtk::Widget>()
            };
            let name = widgets::name_cell(agent, model, pill);
            name.set_tooltip_text(Some(&format!(
                "{} · {}\n{} sessions · last used {}",
                agent.label(),
                model,
                g.sessions,
                format::ago(g.last_ts, chrono::Utc::now().timestamp_millis())
            )));
            table.row(vec![
                name.upcast(),
                num(format::count(g.totals.requests)),
                num(format::tokens(g.totals.tokens.total())),
                num(format::usd_exact(g.totals.usd)),
                dim(if per_req > 0.0 { format::usd_exact(per_req) } else { "—".into() }),
                dim(g.totals.cache_hit().map(format::percent).unwrap_or_else(|| "—".into())),
                strip::strip(g.speed.tps, tps_scale, agent, tps_full).upcast(),
                num(g.speed.tps.map(|q| format::tps(q.p50)).unwrap_or_else(|| "—".into())),
                strip::strip(g.speed.ttft, ttft_scale, agent, ms_full).upcast(),
                num(g.speed.ttft.map(|q| format::ms(q.p50)).unwrap_or_else(|| "—".into())),
            ]);
        }
        widgets::clear(&self.holder);
        if groups.is_empty() {
            self.holder.append(&widgets::label("No requests in this range", &["empty-note"]));
        } else {
            self.holder.append(&table.grid);
        }
    }
}
