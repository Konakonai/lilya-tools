//! Stacked bars over time (days or weeks), one color per agent.

use std::cell::RefCell;
use std::rc::Rc;

use chrono::NaiveDate;
use gtk::prelude::*;

use super::Slot;
use crate::model::Agent;
use crate::ui::theme::{self, Align, Ink};

pub struct Bar {
    /// Tooltip heading, e.g. "Tue, Nov 25".
    pub title: String,
    /// Day to open on click.
    pub day: Option<NaiveDate>,
    pub segments: Vec<(Agent, f64)>,
    pub total: f64,
    /// Extra tooltip line (markup).
    pub note: String,
}

pub struct Data {
    pub bars: Vec<Bar>,
    /// Bar index and label for the x axis.
    pub labels: Vec<(usize, String)>,
    pub fmt: fn(f64) -> String,
    /// Bar drawn with an outline, e.g. the day open in Today.
    pub mark: Option<usize>,
}

impl Default for Data {
    fn default() -> Data {
        Data { bars: Vec::new(), labels: Vec::new(), fmt: |v| format!("{v:.0}"), mark: None }
    }
}

pub struct Bars {
    pub area: gtk::DrawingArea,
    data: Rc<RefCell<Data>>,
    pub pick: Slot<dyn Fn(NaiveDate)>,
}

const TOP: f64 = 10.0;
const BOTTOM: f64 = 26.0;
const RIGHT: f64 = 2.0;

struct Geo {
    left: f64,
    slot: f64,
    plot_h: f64,
    ymax: f64,
    step: f64,
}

fn geometry(a: &gtk::DrawingArea, d: &Data, w: f64, h: f64) -> Geo {
    let max = d.bars.iter().map(|b| b.total).fold(0.0, f64::max);
    let (ymax, step) = theme::nice_scale(max, 4);
    let t = theme::text(a, 8.0);
    let mut left: f64 = 0.0;
    let mut v = 0.0;
    while v <= ymax + step * 0.01 {
        left = left.max(t.width(&(d.fmt)(v)));
        v += step;
    }
    let left = left + 12.0;
    let n = d.bars.len().max(1) as f64;
    Geo { left, slot: (w - left - RIGHT) / n, plot_h: h - TOP - BOTTOM, ymax, step }
}

fn index(g: &Geo, d: &Data, x: f64) -> Option<usize> {
    let i = ((x - g.left) / g.slot).floor();
    (i >= 0.0 && (i as usize) < d.bars.len()).then_some(i as usize)
}

impl Bars {
    pub fn new(height: i32) -> Bars {
        let area = super::area(height);
        let data: Rc<RefCell<Data>> = Rc::default();
        let pick: Slot<dyn Fn(NaiveDate)> = Rc::default();
        let ptr = super::track(&area);
        {
            let data = data.clone();
            area.set_draw_func(move |a, cr, w, h| {
                let d = data.borrow();
                let ink = Ink::of(a);
                let (w, h) = (w as f64, h as f64);
                let g = geometry(a, &d, w, h);
                let base = TOP + g.plot_h;
                let t = theme::text(a, 8.0);

                // Grid and y labels.
                let mut v = 0.0;
                while v <= g.ymax + g.step * 0.01 {
                    let y = base - v / g.ymax * g.plot_h;
                    ink.fg.set(cr, if v == 0.0 { 0.16 } else { 0.07 });
                    theme::hline(cr, g.left, w - RIGHT, y);
                    ink.fg.set(cr, 0.5);
                    t.draw(cr, &(d.fmt)(v), g.left - 10.0, y, Align::End, None);
                    v += g.step;
                }

                let hover = ptr.get().and_then(|(x, y)| (y < base + 4.0).then(|| index(&g, &d, x)).flatten());
                let bw = (g.slot * 0.68).clamp(1.0, 46.0);
                for (i, b) in d.bars.iter().enumerate() {
                    let x = g.left + i as f64 * g.slot + (g.slot - bw) / 2.0;
                    if hover == Some(i) {
                        ink.fg.set(cr, 0.05);
                        theme::rounded_rect(cr, g.left + i as f64 * g.slot, TOP - 6.0, g.slot, g.plot_h + 10.0, 5.0);
                        theme::fill(cr);
                    }
                    if b.total <= 0.0 {
                        continue;
                    }
                    let hh = (b.total / g.ymax * g.plot_h).max(1.5);
                    let top = base - hh;
                    cr.save().ok();
                    theme::top_rounded_rect(cr, x, top, bw, hh, (bw / 2.0).min(4.0));
                    cr.clip();
                    let alpha = if hover.is_none() || hover == Some(i) { 0.92 } else { 0.62 };
                    let mut y = base;
                    for (agent, v) in &b.segments {
                        let sh = v / g.ymax * g.plot_h;
                        theme::agent_color(*agent).set(cr, alpha);
                        cr.rectangle(x, y - sh, bw, sh + 0.5);
                        theme::fill(cr);
                        y -= sh;
                    }
                    cr.restore().ok();
                    if d.mark == Some(i) {
                        ink.fg.set(cr, 0.7);
                        cr.set_line_width(1.5);
                        theme::top_rounded_rect(cr, x - 1.5, top - 1.5, bw + 3.0, hh + 1.5, (bw / 2.0).min(4.0) + 1.0);
                        theme::stroke(cr);
                    }
                }

                // X labels, skipping any that would collide.
                ink.fg.set(cr, 0.55);
                let mut last_right = f64::MIN;
                for (i, s) in &d.labels {
                    let cx = g.left + (*i as f64 + 0.5) * g.slot;
                    let lw = t.width(s);
                    let left = (cx - lw / 2.0).max(g.left - 4.0);
                    if left < last_right + 10.0 || left + lw > w + 2.0 {
                        continue;
                    }
                    t.draw(cr, s, left, h - BOTTOM / 2.0 + 3.0, Align::Start, None);
                    last_right = left + lw;
                }
            });
        }
        {
            let data = data.clone();
            super::tooltip(&area, move |a, x, _| {
                let d = data.borrow();
                let g = geometry(a, &d, a.width() as f64, a.height() as f64);
                let b = &d.bars[index(&g, &d, x)?];
                let mut s = format!("<b>{}</b>", super::esc(&b.title));
                for (agent, v) in b.segments.iter().rev() {
                    s.push_str(&format!("\n{}  <b>{}</b>", agent.label(), (d.fmt)(*v)));
                }
                if b.segments.len() > 1 {
                    s.push_str(&format!("\nTotal  <b>{}</b>", (d.fmt)(b.total)));
                }
                if b.total == 0.0 {
                    s.push_str("\nNo activity");
                }
                if !b.note.is_empty() {
                    s.push('\n');
                    s.push_str(&b.note);
                }
                Some(s)
            });
        }
        {
            let data = data.clone();
            super::pointer_cursor(&area, move |a, x, _| {
                let d = data.borrow();
                let g = geometry(a, &d, a.width() as f64, a.height() as f64);
                index(&g, &d, x).is_some_and(|i| d.bars[i].day.is_some())
            });
        }
        {
            let (data, pick) = (data.clone(), pick.clone());
            super::on_click(&area, move |a, x, _| {
                let day = {
                    let d = data.borrow();
                    let g = geometry(a, &d, a.width() as f64, a.height() as f64);
                    index(&g, &d, x).and_then(|i| d.bars[i].day)
                };
                if let (Some(day), Some(f)) = (day, pick.borrow().as_ref()) {
                    f(day);
                }
            });
        }
        Bars { area, data, pick }
    }

    pub fn set(&self, d: Data) {
        *self.data.borrow_mut() = d;
        self.area.queue_draw();
    }
}
