//! Cairo-drawn charts. Each chart owns a DrawingArea plus the data it shows;
//! pages hand it plain values and it redraws.

pub mod bars;
pub mod composition;
pub mod context;
pub mod heatmap;
pub mod meter;
pub mod ranked;
pub mod strip;
pub mod timeline;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;

/// Last pointer position over a chart, for hover highlights.
pub type Pointer = Rc<Cell<Option<(f64, f64)>>>;

/// A callback slot a page fills in after building the chart.
pub type Slot<T> = Rc<RefCell<Option<Box<T>>>>;

pub fn area(height: i32) -> gtk::DrawingArea {
    let a = gtk::DrawingArea::new();
    a.add_css_class("chart");
    a.set_content_height(height);
    a.set_hexpand(true);
    a
}

/// Tracks the pointer and redraws on movement.
pub fn track(area: &gtk::DrawingArea) -> Pointer {
    let p: Pointer = Rc::new(Cell::new(None));
    let motion = gtk::EventControllerMotion::new();
    {
        let (p, a) = (p.clone(), area.downgrade());
        motion.connect_motion(move |_, x, y| {
            p.set(Some((x, y)));
            if let Some(a) = a.upgrade() {
                a.queue_draw();
            }
        });
    }
    {
        let (p, a) = (p.clone(), area.downgrade());
        motion.connect_leave(move |_| {
            p.set(None);
            if let Some(a) = a.upgrade() {
                a.queue_draw();
            }
        });
    }
    area.add_controller(motion);
    p
}

/// Shows a hand cursor where `hit` says something can be clicked.
pub fn pointer_cursor(area: &gtk::DrawingArea, hit: impl Fn(&gtk::DrawingArea, f64, f64) -> bool + 'static) {
    let motion = gtk::EventControllerMotion::new();
    let a = area.downgrade();
    motion.connect_motion(move |_, x, y| {
        if let Some(a) = a.upgrade() {
            a.set_cursor_from_name(if hit(&a, x, y) { Some("pointer") } else { None });
        }
    });
    area.add_controller(motion);
}

pub fn on_click(area: &gtk::DrawingArea, f: impl Fn(&gtk::DrawingArea, f64, f64) + 'static) {
    let g = gtk::GestureClick::new();
    g.set_button(gtk::gdk::BUTTON_PRIMARY);
    g.connect_released(move |g, _, x, y| {
        if let Some(w) = g.widget().and_downcast::<gtk::DrawingArea>() {
            f(&w, x, y);
        }
    });
    area.add_controller(g);
}

/// Tooltip markup computed from the pointer position.
pub fn tooltip(area: &gtk::DrawingArea, f: impl Fn(&gtk::DrawingArea, f64, f64) -> Option<String> + 'static) {
    area.set_has_tooltip(true);
    area.connect_query_tooltip(move |a, x, y, _, tip| match f(a, x as f64, y as f64) {
        Some(m) => {
            tip.set_markup(Some(&m));
            true
        }
        None => false,
    });
}

pub fn esc(s: &str) -> String {
    glib::markup_escape_text(s).to_string()
}
