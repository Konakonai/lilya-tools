//! A year of days as a calendar heatmap: weeks are columns, Monday on top.

use std::cell::RefCell;
use std::rc::Rc;

use chrono::{Datelike, NaiveDate};
use gtk::prelude::*;

use super::Slot;
use crate::ui::theme::{self, Align, Ink};

pub struct Day {
    pub date: NaiveDate,
    pub value: f64,
    /// Tooltip (markup).
    pub tip: String,
}

#[derive(Default)]
pub struct Data {
    /// Consecutive days; the first is a Monday.
    pub days: Vec<Day>,
    /// Upper bounds of intensity levels 1–3; anything above is level 4.
    pub levels: [f64; 3],
    pub today: Option<NaiveDate>,
    pub mark: Option<NaiveDate>,
}

pub struct Heatmap {
    pub area: gtk::DrawingArea,
    data: Rc<RefCell<Data>>,
    pub pick: Slot<dyn Fn(NaiveDate)>,
}

const LEFT: f64 = 32.0;
const TOP: f64 = 20.0;
const BOTTOM: f64 = 26.0;
const GAP: f64 = 3.0;
const ALPHAS: [f64; 5] = [0.0, 0.22, 0.42, 0.66, 0.95];

fn weeks(d: &Data) -> usize {
    d.days.len().div_ceil(7).max(1)
}

fn cell(d: &Data, w: f64) -> f64 {
    ((w - LEFT) / weeks(d) as f64 - GAP).clamp(6.0, 18.0)
}

pub fn height_for(d: &Data, w: f64) -> i32 {
    (TOP + 7.0 * (cell(d, w) + GAP) + BOTTOM).ceil() as i32
}

fn index(d: &Data, w: f64, x: f64, y: f64) -> Option<usize> {
    let c = cell(d, w) + GAP;
    let (col, row) = (((x - LEFT) / c).floor(), ((y - TOP) / c).floor());
    if col < 0.0 || !(0.0..7.0).contains(&row) {
        return None;
    }
    let i = col as usize * 7 + row as usize;
    (i < d.days.len() && d.today.is_none_or(|t| d.days[i].date <= t)).then_some(i)
}

fn level(d: &Data, v: f64) -> usize {
    if v <= 0.0 { 0 } else { 1 + d.levels.iter().filter(|&&l| v > l).count() }
}

impl Heatmap {
    pub fn new() -> Heatmap {
        let area = super::area(150);
        let data: Rc<RefCell<Data>> = Rc::default();
        let pick: Slot<dyn Fn(NaiveDate)> = Rc::default();
        let ptr = super::track(&area);
        {
            let data = data.clone();
            area.set_draw_func(move |a, cr, w, _h| {
                let d = data.borrow();
                let ink = Ink::of(a);
                let w = w as f64;
                let c = cell(&d, w);
                let t = theme::text(a, 7.5);
                let hover = ptr.get().and_then(|(x, y)| index(&d, w, x, y));

                ink.fg.set(cr, 0.5);
                for (row, name) in [(0, "Mon"), (2, "Wed"), (4, "Fri")] {
                    t.draw(cr, name, 0.0, TOP + row as f64 * (c + GAP) + c / 2.0, Align::Start, None);
                }
                let mut last_label = f64::MIN;
                let mut prev_month = None;
                for (i, day) in d.days.iter().enumerate() {
                    let (col, row) = (i / 7, i % 7);
                    let x = LEFT + col as f64 * (c + GAP);
                    let y = TOP + row as f64 * (c + GAP);
                    if row == 0 {
                        let m = day.date.month();
                        if prev_month.is_some_and(|p| p != m) && x > last_label + 28.0 {
                            ink.fg.set(cr, 0.55);
                            t.draw(cr, &day.date.format("%b").to_string(), x, TOP / 2.0 - 1.0, Align::Start, None);
                            last_label = x;
                        }
                        prev_month = Some(m);
                    }
                    if d.today.is_some_and(|t| day.date > t) {
                        continue;
                    }
                    let lv = level(&d, day.value);
                    if lv == 0 {
                        ink.fg.set(cr, 0.07);
                    } else {
                        ink.accent.set(cr, ALPHAS[lv]);
                    }
                    theme::rounded_rect(cr, x, y, c, c, 3.0);
                    theme::fill(cr);
                    if hover == Some(i) || d.mark == Some(day.date) {
                        ink.fg.set(cr, if hover == Some(i) { 0.75 } else { 0.45 });
                        cr.set_line_width(1.5);
                        theme::rounded_rect(cr, x - 1.0, y - 1.0, c + 2.0, c + 2.0, 4.0);
                        theme::stroke(cr);
                    }
                }

                // Legend.
                let ly = TOP + 7.0 * (c + GAP) + BOTTOM / 2.0 + 2.0;
                let s = 10.0;
                let grid_right = LEFT + weeks(&d) as f64 * (c + GAP) - GAP;
                let mut x = grid_right - 5.0 * (s + 3.0) - t.width("More") - 3.0;
                ink.fg.set(cr, 0.5);
                t.draw(cr, "Less", x - 6.0, ly, Align::End, None);
                for (lv, alpha) in ALPHAS.iter().enumerate() {
                    if lv == 0 {
                        ink.fg.set(cr, 0.07);
                    } else {
                        ink.accent.set(cr, *alpha);
                    }
                    theme::rounded_rect(cr, x, ly - s / 2.0, s, s, 2.5);
                    theme::fill(cr);
                    x += s + 3.0;
                }
                ink.fg.set(cr, 0.5);
                t.draw(cr, "More", x + 3.0, ly, Align::Start, None);
            });
        }
        {
            let data = data.clone();
            area.connect_resize(move |a, w, _| {
                let h = height_for(&data.borrow(), w as f64);
                if a.content_height() != h {
                    a.set_content_height(h);
                }
            });
        }
        {
            let data = data.clone();
            super::tooltip(&area, move |a, x, y| {
                let d = data.borrow();
                let i = index(&d, a.width() as f64, x, y)?;
                Some(d.days[i].tip.clone())
            });
        }
        {
            let data = data.clone();
            super::pointer_cursor(&area, move |a, x, y| index(&data.borrow(), a.width() as f64, x, y).is_some());
        }
        {
            let (data, pick) = (data.clone(), pick.clone());
            super::on_click(&area, move |a, x, y| {
                let day = {
                    let d = data.borrow();
                    index(&d, a.width() as f64, x, y).map(|i| d.days[i].date)
                };
                if let (Some(day), Some(f)) = (day, pick.borrow().as_ref()) {
                    f(day);
                }
            });
        }
        Heatmap { area, data, pick }
    }

    pub fn set(&self, d: Data) {
        let h = height_for(&d, self.area.width().max(600) as f64);
        *self.data.borrow_mut() = d;
        self.area.set_content_height(h);
        self.area.queue_draw();
    }
}
