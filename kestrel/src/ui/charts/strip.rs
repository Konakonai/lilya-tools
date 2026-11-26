//! Distribution strips: p10–p90 thin, p25–p75 thick, a tick at the median.
//! Rows of a table share one [`Scale`] so they can be compared by eye.

use gtk::prelude::*;

use crate::analytics::Quantiles;
use crate::model::Agent;
use crate::ui::theme::{self, Align, Ink};

#[derive(Clone, Copy)]
pub enum Scale {
    Linear(f64),
    Log(f64, f64),
}

impl Scale {
    /// Position of `v` on the axis, 0..1.
    pub fn pos(self, v: f64) -> f64 {
        match self {
            Scale::Linear(max) => (v / max.max(1e-9)).clamp(0.0, 1.0),
            Scale::Log(lo, hi) => ((v.max(lo).ln() - lo.ln()) / (hi.ln() - lo.ln()).max(1e-9)).clamp(0.0, 1.0),
        }
    }
}

const PAD: f64 = 8.0;
pub const WIDTH: i32 = 168;

pub fn strip(q: Option<Quantiles>, scale: Scale, agent: Agent, fmt: fn(f64) -> String) -> gtk::DrawingArea {
    let area = super::area(22);
    area.set_content_width(WIDTH);
    area.set_hexpand(false);
    area.set_draw_func(move |a, cr, w, h| {
        let ink = Ink::of(a);
        let (w, h) = (w as f64, h as f64);
        let cy = (h / 2.0).round();
        let span = w - 2.0 * PAD;
        let x = |v: f64| PAD + scale.pos(v) * span;
        ink.fg.set(cr, 0.07);
        theme::rounded_rect(cr, PAD, cy - 1.0, span, 2.0, 1.0);
        theme::fill(cr);
        let Some(q) = q else { return };
        let color = theme::agent_color(agent);
        color.set(cr, 0.32);
        theme::rounded_rect(cr, x(q.p10) - 2.0, cy - 2.0, (x(q.p90) - x(q.p10)).max(0.0) + 4.0, 4.0, 2.0);
        theme::fill(cr);
        color.set(cr, 0.88);
        theme::rounded_rect(cr, x(q.p25) - 3.0, cy - 4.0, (x(q.p75) - x(q.p25)).max(0.0) + 6.0, 8.0, 4.0);
        theme::fill(cr);
        ink.fg.set(cr, 0.9);
        theme::rounded_rect(cr, x(q.p50) - 1.25, cy - 7.0, 2.5, 14.0, 1.25);
        theme::fill(cr);
    });
    if let Some(q) = q {
        area.set_tooltip_markup(Some(&format!(
            "Median <b>{}</b>\nMiddle half {} – {}\n10th–90th percentile {} – {}\n<small>{} requests measured</small>",
            fmt(q.p50),
            fmt(q.p25),
            fmt(q.p75),
            fmt(q.p10),
            fmt(q.p90),
            q.n
        )));
    }
    area
}

/// Tick labels for a column of strips.
pub fn axis(scale: Scale, ticks: Vec<f64>, fmt: fn(f64) -> String) -> gtk::DrawingArea {
    let area = super::area(16);
    area.set_content_width(WIDTH);
    area.set_hexpand(false);
    area.set_draw_func(move |a, cr, w, h| {
        let ink = Ink::of(a);
        let span = w as f64 - 2.0 * PAD;
        let t = theme::text(a, 7.5);
        ink.fg.set(cr, 0.5);
        let n = ticks.len();
        for (i, &v) in ticks.iter().enumerate() {
            let x = PAD + scale.pos(v) * span;
            let align = if i == 0 && x < PAD + 12.0 {
                Align::Start
            } else if i + 1 == n && x > w as f64 - PAD - 12.0 {
                Align::End
            } else {
                Align::Center
            };
            t.draw(cr, &fmt(v), x, h as f64 / 2.0, align, None);
        }
    });
    area
}
