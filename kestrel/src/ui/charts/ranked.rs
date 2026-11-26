//! A ranked list of horizontal bars: name, bar, value.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;

use crate::model::Agent;
use crate::ui::theme::{self, Align, Ink};

pub struct Item {
    pub label: String,
    /// Tooltip body (markup).
    pub detail: String,
    pub value: f64,
    pub text: String,
    pub agent: Agent,
}

pub struct Ranked {
    pub area: gtk::DrawingArea,
    items: Rc<RefCell<Vec<Item>>>,
}

const ROW: f64 = 30.0;

fn index(items: &[Item], y: f64) -> Option<usize> {
    let i = (y / ROW).floor();
    (i >= 0.0 && (i as usize) < items.len()).then_some(i as usize)
}

impl Ranked {
    pub fn new() -> Ranked {
        let area = super::area(ROW as i32);
        let items: Rc<RefCell<Vec<Item>>> = Rc::default();
        let ptr = super::track(&area);
        {
            let items = items.clone();
            area.set_draw_func(move |a, cr, w, h| {
                let items = items.borrow();
                let ink = Ink::of(a);
                let (w, h) = (w as f64, h as f64);
                if items.is_empty() {
                    ink.fg.set(cr, 0.45);
                    theme::text(a, 9.5).draw(cr, "Nothing in this range", w / 2.0, h / 2.0, Align::Center, None);
                    return;
                }
                let max = items.iter().map(|i| i.value).fold(0.0, f64::max);
                let t = theme::text(a, 9.5);
                let tv = theme::bold(a, 9.5);
                let value_w = items.iter().map(|i| tv.width(&i.text)).fold(0.0, f64::max) + 8.0;
                let label_w = (w * 0.38).clamp(90.0, 240.0);
                let bar_x = label_w + 10.0;
                let bar_w = (w - bar_x - value_w - 10.0).max(12.0);
                let hover = ptr.get().and_then(|(_, y)| index(&items, y));
                for (i, it) in items.iter().enumerate() {
                    let y = i as f64 * ROW;
                    let cy = y + ROW / 2.0;
                    if hover == Some(i) {
                        ink.fg.set(cr, 0.05);
                        theme::rounded_rect(cr, 0.0, y + 1.0, w, ROW - 2.0, 7.0);
                        theme::fill(cr);
                    }
                    ink.fg.set(cr, 0.88);
                    t.draw(cr, &it.label, 8.0, cy, Align::Start, Some(label_w - 8.0));
                    ink.fg.set(cr, 0.06);
                    theme::rounded_rect(cr, bar_x, cy - 4.0, bar_w, 8.0, 4.0);
                    theme::fill(cr);
                    let frac = if max > 0.0 { it.value / max } else { 0.0 };
                    theme::agent_color(it.agent).set(cr, 0.85);
                    theme::rounded_rect(cr, bar_x, cy - 4.0, (bar_w * frac).max(4.0), 8.0, 4.0);
                    theme::fill(cr);
                    ink.fg.set(cr, 0.85);
                    tv.draw(cr, &it.text, w - 8.0, cy, Align::End, None);
                }
            });
        }
        {
            let items = items.clone();
            super::tooltip(&area, move |_, _, y| {
                let items = items.borrow();
                let it = &items[index(&items, y)?];
                Some(format!("<b>{}</b>\n{}", super::esc(&it.label), it.detail))
            });
        }
        Ranked { area, items }
    }

    pub fn set(&self, items: Vec<Item>) {
        let n = items.len().max(2);
        *self.items.borrow_mut() = items;
        self.area.set_content_height((n as f64 * ROW) as i32);
        self.area.queue_draw();
    }
}
