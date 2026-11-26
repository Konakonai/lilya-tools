//! A thin bar splitting tokens into cache reads, cache writes, new input and output.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;

use crate::format;
use crate::model::Tokens;
use crate::ui::theme::{self, Ink};

pub struct Composition {
    pub area: gtk::DrawingArea,
    data: Rc<RefCell<Tokens>>,
}

fn parts(t: &Tokens) -> [u64; 4] {
    [t.cache_read, t.cache_write + t.cache_write_1h, t.input, t.output]
}

impl Composition {
    pub fn new() -> Composition {
        let area = super::area(8);
        area.set_margin_top(7);
        area.set_margin_bottom(3);
        let data: Rc<RefCell<Tokens>> = Rc::default();
        {
            let data = data.clone();
            area.set_draw_func(move |a, cr, w, h| {
                let t = *data.borrow();
                let ink = Ink::of(a);
                let (w, h) = (w as f64, h as f64);
                theme::rounded_rect(cr, 0.0, 0.0, w, h, h / 2.0);
                cr.clip();
                let total = t.total() as f64;
                if total <= 0.0 {
                    ink.fg.set(cr, 0.08);
                    let _ = cr.paint();
                    return;
                }
                let inks = theme::token_inks(&ink);
                let mut x = 0.0;
                for (v, (rgb, alpha)) in parts(&t).into_iter().zip(inks) {
                    if v == 0 {
                        continue;
                    }
                    let sw = v as f64 / total * w;
                    rgb.set(cr, alpha);
                    // A hairline gap keeps adjacent tints apart.
                    cr.rectangle(x, 0.0, (sw - 1.0).max(1.0), h);
                    theme::fill(cr);
                    x += sw;
                }
            });
        }
        {
            let data = data.clone();
            super::tooltip(&area, move |_, _, _| {
                let t = *data.borrow();
                let total = t.total();
                if total == 0 {
                    return None;
                }
                let mut lines = Vec::new();
                for (name, v) in theme::TOKEN_NAMES.iter().zip(parts(&t)) {
                    lines.push(format!("{name}  <b>{}</b>  {}", format::tokens(v), format::percent(v as f64 / total as f64)));
                }
                if t.reasoning > 0 {
                    lines.push(format!("<small>Output includes {} reasoning</small>", format::tokens(t.reasoning)));
                }
                Some(lines.join("\n"))
            });
        }
        Composition { area, data }
    }

    pub fn set(&self, t: Tokens) {
        *self.data.borrow_mut() = t;
        self.area.queue_draw();
    }
}
