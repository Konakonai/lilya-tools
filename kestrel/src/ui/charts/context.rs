//! One session, request by request: the prompt split by cache state, output on
//! top, and a running cost line on its own scale.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;

use crate::format;
use crate::model::Call;
use crate::ui::theme::{self, Align, Ink};

pub struct Data {
    pub calls: Vec<Call>,
}

pub struct ContextChart {
    pub area: gtk::DrawingArea,
}

const TOP: f64 = 30.0;
const BOTTOM: f64 = 22.0;

fn parts(c: &Call) -> [f64; 4] {
    let t = &c.tokens;
    [t.cache_read as f64, (t.cache_write + t.cache_write_1h) as f64, t.input as f64, t.output as f64]
}

struct Geo {
    left: f64,
    right: f64,
    slot: f64,
    plot_h: f64,
    ymax: f64,
    ystep: f64,
    cmax: f64,
}

fn geometry(a: &gtk::DrawingArea, d: &Data, w: f64, h: f64) -> Geo {
    let max = d.calls.iter().map(|c| parts(c).iter().sum::<f64>()).fold(0.0, f64::max);
    let (ymax, ystep) = theme::nice_scale(max, 4);
    let cost: f64 = d.calls.iter().map(|c| c.usd).sum();
    let (cmax, _) = theme::nice_scale(cost, 4);
    let t = theme::text(a, 8.0);
    let left = t.width(&format::tokens(ymax as u64)) + 12.0;
    let right = if cost > 0.0 { t.width(&format::usd(cmax)) + 12.0 } else { 8.0 };
    let n = d.calls.len().max(1) as f64;
    Geo { left, right, slot: (w - left - right) / n, plot_h: h - TOP - BOTTOM, ymax, ystep, cmax }
}

fn index(g: &Geo, d: &Data, x: f64) -> Option<usize> {
    let i = ((x - g.left) / g.slot).floor();
    (i >= 0.0 && (i as usize) < d.calls.len()).then_some(i as usize)
}

impl ContextChart {
    pub fn new(d: Data) -> ContextChart {
        let area = super::area(240);
        let data = Rc::new(RefCell::new(d));
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
                let inks = theme::token_inks(&ink);

                // Legend.
                let mut lx = g.left;
                for (name, (rgb, alpha)) in theme::TOKEN_NAMES.iter().zip(inks) {
                    rgb.set(cr, alpha);
                    theme::rounded_rect(cr, lx, 6.0, 10.0, 10.0, 3.0);
                    theme::fill(cr);
                    ink.fg.set(cr, 0.7);
                    lx += 16.0 + t.draw(cr, name, lx + 15.0, 11.0, Align::Start, None) + 14.0;
                }
                ink.fg.set(cr, 0.8);
                cr.set_line_width(2.0);
                cr.move_to(lx, 11.0);
                cr.line_to(lx + 12.0, 11.0);
                theme::stroke(cr);
                ink.fg.set(cr, 0.7);
                t.draw(cr, "Running cost", lx + 17.0, 11.0, Align::Start, None);

                // Grid.
                let mut v = 0.0;
                while v <= g.ymax + g.ystep * 0.01 {
                    let y = base - v / g.ymax * g.plot_h;
                    ink.fg.set(cr, if v == 0.0 { 0.16 } else { 0.07 });
                    theme::hline(cr, g.left, w - g.right, y);
                    ink.fg.set(cr, 0.5);
                    t.draw(cr, &format::tokens(v as u64), g.left - 8.0, y, Align::End, None);
                    if g.cmax > 0.0 {
                        t.draw(cr, &format::usd(v / g.ymax * g.cmax), w - g.right + 8.0, y, Align::Start, None);
                    }
                    v += g.ystep;
                }

                let hover = ptr.get().and_then(|(x, _)| index(&g, &d, x));
                let bw = (g.slot * 0.8).clamp(0.6, 18.0);
                for (i, c) in d.calls.iter().enumerate() {
                    let x = g.left + i as f64 * g.slot + (g.slot - bw) / 2.0;
                    if hover == Some(i) {
                        ink.fg.set(cr, 0.06);
                        cr.rectangle(g.left + i as f64 * g.slot, TOP - 4.0, g.slot.max(1.0), g.plot_h + 4.0);
                        theme::fill(cr);
                    }
                    let mut y = base;
                    for (v, (rgb, alpha)) in parts(c).into_iter().zip(inks) {
                        if v <= 0.0 {
                            continue;
                        }
                        let sh = v / g.ymax * g.plot_h;
                        rgb.set(cr, alpha);
                        cr.rectangle(x, y - sh, bw, sh);
                        theme::fill(cr);
                        y -= sh;
                    }
                }

                // Running cost.
                if g.cmax > 0.0 {
                    let mut acc = 0.0;
                    ink.fg.set(cr, 0.8);
                    cr.set_line_width(2.0);
                    cr.set_line_join(gtk::cairo::LineJoin::Round);
                    cr.move_to(g.left, base);
                    for (i, c) in d.calls.iter().enumerate() {
                        acc += c.usd;
                        cr.line_to(g.left + (i as f64 + 1.0) * g.slot, base - acc / g.cmax * g.plot_h);
                    }
                    theme::stroke(cr);
                }

                // X labels: first, middle and last request times.
                ink.fg.set(cr, 0.5);
                let n = d.calls.len();
                if n > 0 {
                    let label = |i: usize| format::clock(d.calls[i].ts);
                    t.draw(cr, &label(0), g.left, h - BOTTOM / 2.0 + 2.0, Align::Start, None);
                    if n > 2 {
                        t.draw(
                            cr,
                            &format!("request {}", n / 2 + 1),
                            g.left + (n / 2) as f64 * g.slot,
                            h - BOTTOM / 2.0 + 2.0,
                            Align::Center,
                            None,
                        );
                    }
                    if n > 1 {
                        t.draw(cr, &label(n - 1), w - g.right, h - BOTTOM / 2.0 + 2.0, Align::End, None);
                    }
                }
            });
        }
        {
            let data = data.clone();
            super::tooltip(&area, move |a, x, _| {
                let d = data.borrow();
                let g = geometry(a, &d, a.width() as f64, a.height() as f64);
                let i = index(&g, &d, x)?;
                let c = &d.calls[i];
                let p = parts(c);
                let mut s = format!(
                    "<b>Request {} · {}</b>\n{}\nContext <b>{}</b>  ({} cached, {} written, {} new)\nOutput <b>{}</b>",
                    i + 1,
                    format::clock(c.ts),
                    super::esc(&c.model),
                    format::tokens(c.tokens.prompt()),
                    format::tokens(p[0] as u64),
                    format::tokens(p[1] as u64),
                    format::tokens(p[2] as u64),
                    format::tokens(c.tokens.output),
                );
                if let Some(t) = c.ttft_ms {
                    s.push_str(&format!("\nFirst token after {}", format::ms(t as f64)));
                }
                if let Some(v) = c.tps() {
                    s.push_str(&format!(" · {} tok/s", format::tps(v)));
                }
                s.push_str(&format!("\nCost <b>{}</b>", format::usd_exact(c.usd)));
                let before: f64 = d.calls[..=i].iter().map(|c| c.usd).sum();
                s.push_str(&format!(" · {} so far", format::usd(before)));
                Some(s)
            });
        }
        ContextChart { area }
    }
}
