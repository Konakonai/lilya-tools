//! The day at a glance: a request pulse on top, then one lane per session with
//! its span and a tick for every request, and a "now" line while it is today.
//! A lane with an empty id stands for several sessions and is not clickable.

use std::cell::RefCell;
use std::rc::Rc;

use chrono::Timelike;
use gtk::prelude::*;

use super::Slot;
use crate::analytics;
use crate::format;
use crate::model::Agent;
use crate::ui::theme::{self, Align, Ink};

pub struct Lane {
    pub agent: Agent,
    pub id: String,
    pub title: String,
    pub cost: String,
    /// Tooltip (markup).
    pub tip: String,
    pub start: i64,
    pub end: i64,
    pub ticks: Vec<i64>,
}

#[derive(Default)]
pub struct Data {
    pub lanes: Vec<Lane>,
    /// Requests per bucket, by agent in `Agent::ALL` order.
    pub buckets: Vec<[f64; 6]>,
    pub bucket_tips: Vec<String>,
    pub bucket_ms: i64,
    pub start: i64,
    pub end: i64,
    pub now: Option<i64>,
    pub empty: String,
}

/// Called with the agent and session id of a clicked lane.
pub type OpenSession = dyn Fn(Agent, &str);

pub struct Timeline {
    pub area: gtk::DrawingArea,
    data: Rc<RefCell<Data>>,
    pub pick: Slot<OpenSession>,
}

const PULSE: f64 = 58.0;
const AXIS: f64 = 28.0;
const LANE: f64 = 30.0;
const BOTTOM: f64 = 4.0;
const RIGHT: f64 = 16.0;

fn gutter(w: f64) -> f64 {
    (w * 0.25).clamp(150.0, 260.0)
}

fn content_height(d: &Data) -> i32 {
    if d.lanes.is_empty() { 170 } else { (PULSE + AXIS + d.lanes.len() as f64 * LANE + BOTTOM) as i32 }
}

enum Hit {
    Lane(usize),
    Bucket(usize),
}

fn hit(d: &Data, w: f64, x: f64, y: f64) -> Option<Hit> {
    if d.lanes.is_empty() {
        return None;
    }
    if y >= PULSE + AXIS {
        let i = ((y - PULSE - AXIS) / LANE).floor() as usize;
        return (i < d.lanes.len()).then_some(Hit::Lane(i));
    }
    let g = gutter(w);
    if y < PULSE && x >= g && x <= w - RIGHT && d.bucket_ms > 0 {
        let t = d.start + ((x - g) / (w - RIGHT - g) * (d.end - d.start) as f64) as i64;
        let i = ((t - d.start) / d.bucket_ms) as usize;
        return (i < d.buckets.len() && d.buckets[i].iter().sum::<f64>() > 0.0).then_some(Hit::Bucket(i));
    }
    None
}

/// Tick spacing for the hour axis, in minutes.
fn tick_minutes(span_ms: i64) -> i64 {
    match span_ms / 3_600_000 {
        0..=4 => 30,
        5..=10 => 60,
        11..=16 => 120,
        _ => 180,
    }
}

impl Timeline {
    pub fn new() -> Timeline {
        let area = super::area(170);
        let data: Rc<RefCell<Data>> = Rc::default();
        let pick: Slot<OpenSession> = Rc::default();
        let ptr = super::track(&area);
        {
            let data = data.clone();
            area.set_draw_func(move |a, cr, w, h| {
                let d = data.borrow();
                let ink = Ink::of(a);
                let (w, h) = (w as f64, h as f64);
                if d.lanes.is_empty() {
                    ink.fg.set(cr, 0.45);
                    theme::text(a, 10.5).draw(cr, &d.empty, w / 2.0, h / 2.0, Align::Center, None);
                    return;
                }
                let g = gutter(w);
                let (x0, x1) = (g, w - RIGHT);
                let span = (d.end - d.start).max(1) as f64;
                let px = |t: i64| x0 + (t - d.start) as f64 / span * (x1 - x0);
                let lanes_top = PULSE + AXIS;
                let small = theme::text(a, 8.0);
                let hover = ptr.get().and_then(|(x, y)| hit(&d, w, x, y));
                let now_x = d.now.filter(|n| (d.start..=d.end).contains(n)).map(px);

                // Hour grid and labels.
                let step = tick_minutes(d.end - d.start) * 60_000;
                let first = {
                    let l = analytics::local(d.start);
                    let past = (l.minute() as i64 * 60_000 + l.hour() as i64 * 3_600_000) % step;
                    if past == 0 { d.start } else { d.start + step - past }
                };
                let mut t = first;
                while t <= d.end {
                    let x = px(t);
                    ink.fg.set(cr, 0.06);
                    theme::vline(cr, x, 6.0, h - BOTTOM);
                    if now_x.is_none_or(|n| (n - x).abs() > 30.0) {
                        ink.fg.set(cr, 0.5);
                        small.draw(cr, &format::clock(t), x, PULSE + AXIS / 2.0, Align::Center, None);
                    }
                    t += step;
                }

                // Pulse: requests per bucket, stacked by agent.
                let base = PULSE - 6.0;
                let max = d.buckets.iter().map(|b| b.iter().sum::<f64>()).fold(0.0, f64::max);
                ink.fg.set(cr, 0.14);
                theme::hline(cr, x0, x1, base);
                ink.fg.set(cr, 0.72);
                theme::bold(a, 8.5).draw(cr, "Requests", 8.0, base - 26.0, Align::Start, None);
                ink.fg.set(cr, 0.45);
                small.draw(cr, &format!("peak {} / {} min", max as u64, d.bucket_ms / 60_000), 8.0, base - 11.0, Align::Start, None);
                if max > 0.0 {
                    for (i, b) in d.buckets.iter().enumerate() {
                        let total: f64 = b.iter().sum();
                        if total <= 0.0 {
                            continue;
                        }
                        let bs = d.start + i as i64 * d.bucket_ms;
                        let (bx0, bx1) = (px(bs), px(bs + d.bucket_ms).min(x1));
                        let bw = (bx1 - bx0 - 1.5).max(1.0);
                        let hh = (total / max * (base - 10.0)).max(2.0);
                        cr.save().ok();
                        theme::top_rounded_rect(cr, bx0 + 0.75, base - hh, bw, hh, 2.0);
                        cr.clip();
                        let alpha = if matches!(hover, Some(Hit::Bucket(j)) if j == i) { 1.0 } else { 0.8 };
                        let mut y = base;
                        for (k, v) in b.iter().enumerate() {
                            if *v <= 0.0 {
                                continue;
                            }
                            let sh = v / total * hh;
                            theme::agent_color(Agent::ALL[k]).set(cr, alpha);
                            cr.rectangle(bx0, y - sh, bw + 2.0, sh + 0.5);
                            theme::fill(cr);
                            y -= sh;
                        }
                        cr.restore().ok();
                    }
                }

                // Session lanes.
                let title = theme::text(a, 9.5);
                let cost = theme::text(a, 8.5);
                for (i, lane) in d.lanes.iter().enumerate() {
                    let y = lanes_top + i as f64 * LANE;
                    let cy = y + LANE / 2.0;
                    let color = theme::agent_color(lane.agent);
                    if matches!(hover, Some(Hit::Lane(j)) if j == i) {
                        ink.fg.set(cr, 0.055);
                        theme::rounded_rect(cr, 0.0, y + 1.0, w, LANE - 2.0, 8.0);
                        theme::fill(cr);
                    }
                    color.set(cr, 1.0);
                    cr.arc(12.0, cy, 4.0, 0.0, std::f64::consts::TAU);
                    theme::fill(cr);
                    // Narrow windows drop the cost column; it is in the tooltip.
                    ink.fg.set(cr, 0.5);
                    let cw = if g >= 200.0 { cost.draw(cr, &lane.cost, g - 14.0, cy, Align::End, None) } else { -10.0 };
                    ink.fg.set(cr, 0.9);
                    title.draw(cr, &lane.title, 24.0, cy, Align::Start, Some((g - 24.0 - cw - 24.0).max(20.0)));

                    let (sx, ex) = (px(lane.start), px(lane.end));
                    let ex = ex.max(sx + 7.0);
                    color.set(cr, 0.17);
                    theme::rounded_rect(cr, sx - 3.0, cy - 8.0, ex - sx + 6.0, 16.0, 8.0);
                    theme::fill(cr);
                    color.set(cr, 0.95);
                    let mut last = f64::MIN;
                    for &t in &lane.ticks {
                        let x = px(t);
                        if x - last < 1.2 {
                            continue;
                        }
                        cr.rectangle(x - 0.75, cy - 5.0, 1.5, 10.0);
                        last = x;
                    }
                    theme::fill(cr);
                }

                // Now.
                if let (Some(x), Some(n)) = (now_x, d.now) {
                    ink.accent.set(cr, 0.9);
                    cr.rectangle(x.round() - 0.75, 6.0, 1.5, h - BOTTOM - 6.0);
                    theme::fill(cr);
                    let label = format::clock(n);
                    let b = theme::bold(a, 8.0);
                    let lw = b.width(&label) + 12.0;
                    let lx = (x - lw / 2.0).clamp(x0 - 4.0, w - lw);
                    ink.accent.set(cr, 1.0);
                    theme::rounded_rect(cr, lx, PULSE + AXIS / 2.0 - 8.5, lw, 17.0, 8.5);
                    theme::fill(cr);
                    cr.set_source_rgb(1.0, 1.0, 1.0);
                    b.draw(cr, &label, lx + lw / 2.0, PULSE + AXIS / 2.0, Align::Center, None);
                }
            });
        }
        {
            let data = data.clone();
            super::tooltip(&area, move |a, x, y| {
                let d = data.borrow();
                match hit(&d, a.width() as f64, x, y)? {
                    Hit::Lane(i) => Some(d.lanes[i].tip.clone()),
                    Hit::Bucket(i) => d.bucket_tips.get(i).cloned(),
                }
            });
        }
        {
            let data = data.clone();
            super::pointer_cursor(&area, move |a, x, y| {
                let d = data.borrow();
                matches!(hit(&d, a.width() as f64, x, y), Some(Hit::Lane(i)) if !d.lanes[i].id.is_empty())
            });
        }
        {
            let (data, pick) = (data.clone(), pick.clone());
            super::on_click(&area, move |a, x, y| {
                let target = {
                    let d = data.borrow();
                    match hit(&d, a.width() as f64, x, y) {
                        Some(Hit::Lane(i)) if !d.lanes[i].id.is_empty() => Some((d.lanes[i].agent, d.lanes[i].id.clone())),
                        _ => None,
                    }
                };
                if let (Some((agent, id)), Some(f)) = (target, pick.borrow().as_ref()) {
                    f(agent, &id);
                }
            });
        }
        Timeline { area, data, pick }
    }

    pub fn set(&self, d: Data) {
        self.area.set_content_height(content_height(&d));
        *self.data.borrow_mut() = d;
        self.area.queue_draw();
    }
}
