//! Colors and Cairo/Pango drawing helpers shared by the charts.
//!
//! Agent hues come from Catppuccin (Latte for light, Mocha for dark) so they sit
//! well next to the user's palette; everything else is derived from the
//! widget's own foreground and accent so charts follow the GTK theme.

use gtk::prelude::*;
use gtk::{cairo, gdk, pango};

use crate::model::Agent;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgb(pub f64, pub f64, pub f64);

impl Rgb {
    pub const fn hex(v: u32) -> Rgb {
        Rgb(((v >> 16) & 0xff) as f64 / 255.0, ((v >> 8) & 0xff) as f64 / 255.0, (v & 0xff) as f64 / 255.0)
    }

    pub fn from_rgba(c: &gdk::RGBA) -> Rgb {
        Rgb(c.red() as f64, c.green() as f64, c.blue() as f64)
    }

    pub fn set(self, cr: &cairo::Context, alpha: f64) {
        cr.set_source_rgba(self.0, self.1, self.2, alpha);
    }
}

pub fn is_dark() -> bool {
    adw::StyleManager::default().is_dark()
}

pub fn agent_color(a: Agent) -> Rgb {
    let dark = is_dark();
    let (l, d) = match a {
        Agent::Claude => (0xd9774f, 0xf2a27e),
        Agent::Antigravity => (0x1e66f5, 0x89b4fa),
        Agent::Codex => (0x179299, 0x94e2d5),
        Agent::Grok => (0xd658a6, 0xf5c2e7),
        Agent::Pi => (0xdf8e1d, 0xf9e2af),
        Agent::Magpie => (0x8c8fa1, 0x9399b2),
    };
    Rgb::hex(if dark { d } else { l })
}

fn pick(light: u32, dark: u32) -> Rgb {
    Rgb::hex(if is_dark() { dark } else { light })
}

/// Output tokens: Catppuccin peach, away from the accent used for prompt tokens.
pub fn output_color() -> Rgb {
    pick(0xfe640b, 0xfab387)
}

pub fn warning_color() -> Rgb {
    pick(0xdf8e1d, 0xf9e2af)
}

pub fn error_color() -> Rgb {
    pick(0xd20f39, 0xf38ba8)
}

/// Token kinds as (color, alpha): cache read, cache write, new input, output.
/// Prompt tokens share the accent and get stronger as they get more expensive.
pub fn token_inks(ink: &Ink) -> [(Rgb, f64); 4] {
    [(ink.accent, 0.28), (ink.accent, 0.58), (ink.accent, 0.95), (output_color(), 0.95)]
}

pub const TOKEN_NAMES: [&str; 4] = ["Cache read", "Cache write", "New input", "Output"];

/// Colors read from the widget's computed style.
#[derive(Clone, Copy)]
pub struct Ink {
    pub fg: Rgb,
    pub accent: Rgb,
}

impl Ink {
    /// The chart's own CSS color is the accent (class `.chart`), its parent's the text color.
    pub fn of(w: &impl IsA<gtk::Widget>) -> Ink {
        let accent = Rgb::from_rgba(&w.as_ref().color());
        let fg = w.as_ref().parent().map(|p| Rgb::from_rgba(&p.color())).unwrap_or(Rgb(0.2, 0.2, 0.25));
        Ink { fg, accent }
    }
}

/// Fills the current path, ignoring Cairo errors (a broken surface just draws nothing).
pub fn fill(cr: &cairo::Context) {
    let _ = cr.fill();
}

pub fn stroke(cr: &cairo::Context) {
    let _ = cr.stroke();
}

/// A crisp 1px vertical line.
pub fn vline(cr: &cairo::Context, x: f64, y0: f64, y1: f64) {
    let x = x.round() + 0.5;
    cr.set_line_width(1.0);
    cr.move_to(x, y0);
    cr.line_to(x, y1);
    stroke(cr);
}

/// A crisp 1px horizontal line.
pub fn hline(cr: &cairo::Context, x0: f64, x1: f64, y: f64) {
    let y = y.round() + 0.5;
    cr.set_line_width(1.0);
    cr.move_to(x0, y);
    cr.line_to(x1, y);
    stroke(cr);
}

pub fn rounded_rect(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -std::f64::consts::FRAC_PI_2, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, std::f64::consts::FRAC_PI_2);
    cr.arc(x + r, y + h - r, r, std::f64::consts::FRAC_PI_2, std::f64::consts::PI);
    cr.arc(x + r, y + r, r, std::f64::consts::PI, 1.5 * std::f64::consts::PI);
    cr.close_path();
}

/// Rounds only the top corners, for bars standing on an axis.
pub fn top_rounded_rect(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(w / 2.0).min(h).max(0.0);
    cr.new_sub_path();
    cr.move_to(x, y + h);
    cr.line_to(x, y + r);
    cr.arc(x + r, y + r, r, std::f64::consts::PI, 1.5 * std::f64::consts::PI);
    cr.arc(x + w - r, y + r, r, -std::f64::consts::FRAC_PI_2, 0.0);
    cr.line_to(x + w, y + h);
    cr.close_path();
}

#[derive(Clone, Copy, PartialEq)]
pub enum Align {
    Start,
    Center,
    End,
}

pub struct Text<'a> {
    pub widget: &'a gtk::DrawingArea,
    pub size: f64,
    pub weight: pango::Weight,
}

impl Text<'_> {
    pub fn layout(&self, s: &str) -> pango::Layout {
        let layout = self.widget.create_pango_layout(Some(s));
        let mut font = self.widget.pango_context().font_description().unwrap_or_default();
        font.set_size((self.size * pango::SCALE as f64) as i32);
        font.set_weight(self.weight);
        layout.set_font_description(Some(&font));
        let attrs = pango::AttrList::new();
        attrs.insert(pango::AttrFontFeatures::new("tnum 1"));
        layout.set_attributes(Some(&attrs));
        layout
    }

    /// Draws `s` with its vertical center at `y`. Returns the width used.
    pub fn draw(&self, cr: &cairo::Context, s: &str, x: f64, y: f64, align: Align, max_width: Option<f64>) -> f64 {
        let layout = self.layout(s);
        if let Some(w) = max_width {
            layout.set_width((w * pango::SCALE as f64) as i32);
            layout.set_ellipsize(pango::EllipsizeMode::End);
        }
        let (_, logical) = layout.pixel_extents();
        let w = logical.width() as f64;
        let h = logical.height() as f64;
        let x = match align {
            Align::Start => x,
            Align::Center => x - w / 2.0,
            Align::End => x - w,
        };
        cr.move_to(x, y - h / 2.0);
        pangocairo::functions::show_layout(cr, &layout);
        w
    }

    pub fn width(&self, s: &str) -> f64 {
        self.layout(s).pixel_extents().1.width() as f64
    }
}

pub fn text(widget: &gtk::DrawingArea, size: f64) -> Text<'_> {
    Text { widget, size, weight: pango::Weight::Normal }
}

pub fn bold(widget: &gtk::DrawingArea, size: f64) -> Text<'_> {
    Text { widget, size, weight: pango::Weight::Semibold }
}

/// "Nice" axis maximum and step for `max`, with about `ticks` ticks.
pub fn nice_scale(max: f64, ticks: usize) -> (f64, f64) {
    if max <= 0.0 || !max.is_finite() {
        return (1.0, 0.25);
    }
    let raw = max / ticks as f64;
    let mag = 10f64.powf(raw.log10().floor());
    let norm = raw / mag;
    let step = if norm <= 1.0 {
        1.0
    } else if norm <= 2.0 {
        2.0
    } else if norm <= 2.5 {
        2.5
    } else if norm <= 5.0 {
        5.0
    } else {
        10.0
    } * mag;
    ((max / step).ceil() * step, step)
}
