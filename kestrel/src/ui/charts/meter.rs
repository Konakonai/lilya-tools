//! A quota meter: how much of a rate-limit window is used, with a marker for
//! how much of the window has elapsed, so "ahead of pace" is visible at a glance.

use gtk::prelude::*;

use crate::ui::theme::{self, Ink};

pub fn meter(used: f64, elapsed: Option<f64>) -> gtk::DrawingArea {
    let area = super::area(14);
    area.set_valign(gtk::Align::Center);
    area.set_draw_func(move |a, cr, w, h| {
        let ink = Ink::of(a);
        let (w, h) = (w as f64, h as f64);
        let bh = 8.0;
        let y = (h - bh) / 2.0;
        ink.fg.set(cr, 0.08);
        theme::rounded_rect(cr, 0.0, y, w, bh, bh / 2.0);
        theme::fill(cr);
        let u = used.clamp(0.0, 1.0);
        let color = if u >= 0.95 {
            theme::error_color()
        } else if u >= 0.8 {
            theme::warning_color()
        } else {
            ink.accent
        };
        if u > 0.0 {
            color.set(cr, 1.0);
            theme::rounded_rect(cr, 0.0, y, (w * u).max(bh), bh, bh / 2.0);
            theme::fill(cr);
        }
        if let Some(e) = elapsed {
            let x = (w * e.clamp(0.0, 1.0)).clamp(1.0, w - 1.0);
            ink.fg.set(cr, 0.75);
            theme::rounded_rect(cr, x - 1.0, 0.0, 2.0, h, 1.0);
            theme::fill(cr);
        }
    });
    area
}
