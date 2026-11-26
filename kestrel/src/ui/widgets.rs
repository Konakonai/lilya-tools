//! Small widget builders.

use adw::prelude::*;

use crate::model::Agent;

pub fn label(text: &str, classes: &[&str]) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.set_xalign(0.0);
    for c in classes {
        l.add_css_class(c);
    }
    l
}

pub fn vbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, spacing)
}

pub fn hbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Horizontal, spacing)
}

pub fn clear(b: &gtk::Box) {
    while let Some(c) = b.first_child() {
        b.remove(&c);
    }
}

pub fn spacer() -> gtk::Box {
    let s = hbox(0);
    s.set_hexpand(true);
    s
}

/// A boxed panel using libadwaita's card style.
pub fn card() -> gtk::Box {
    let b = vbox(14);
    b.add_css_class("card");
    b.add_css_class("panel");
    b
}

/// A panel title row; widgets appended to the returned end box sit on the right.
pub fn panel_header(title: &str) -> (gtk::Box, gtk::Box) {
    let row = hbox(12);
    row.append(&label(title, &["panel-title"]));
    row.append(&spacer());
    let end = hbox(14);
    end.set_valign(gtk::Align::Center);
    row.append(&end);
    (row, end)
}

pub fn agent_dot(agent: Agent) -> gtk::Box {
    let d = hbox(0);
    d.add_css_class("agent-dot");
    d.add_css_class(&format!("agent-{}", agent.slug()));
    d.set_valign(gtk::Align::Center);
    d.set_halign(gtk::Align::Center);
    d
}

pub fn legend_chip(agent: Agent, text: &str) -> gtk::Box {
    let b = hbox(6);
    b.add_css_class("legend-chip");
    b.append(&agent_dot(agent));
    b.append(&label(text, &["numeric"]));
    b
}

/// Agent dot plus a model or session name, for the first table column.
pub fn name_cell(agent: Agent, name: &str, pill: Option<&str>) -> gtk::Box {
    let b = hbox(9);
    b.append(&agent_dot(agent));
    let l = label(name, &[]);
    l.set_ellipsize(gtk::pango::EllipsizeMode::End);
    l.set_max_width_chars(42);
    l.set_tooltip_text(Some(name));
    b.append(&l);
    if let Some(p) = pill {
        let pl = label(p, &["pill-unpriced"]);
        pl.set_valign(gtk::Align::Center);
        b.append(&pl);
    }
    b
}

/// Caption, large value with an optional unit, and a detail line.
pub struct Stat {
    pub root: gtk::Box,
    pub value: gtk::Label,
    pub unit: gtk::Label,
    pub detail: gtk::Label,
    pub extra: gtk::Box,
}

impl Stat {
    pub fn set(&self, value: &str, unit: &str, detail: &str) {
        self.value.set_label(value);
        self.unit.set_label(unit);
        self.unit.set_visible(!unit.is_empty());
        self.detail.set_label(detail);
        self.detail.set_visible(!detail.is_empty());
    }
}

pub fn stat(caption: &str) -> Stat {
    let root = vbox(3);
    root.set_hexpand(true);
    root.append(&label(caption, &["eyebrow"]));
    let line = hbox(5);
    let value = label("—", &["stat-value"]);
    value.set_valign(gtk::Align::Baseline);
    let unit = label("", &["stat-unit"]);
    unit.set_valign(gtk::Align::Baseline);
    unit.set_visible(false);
    line.append(&value);
    line.append(&unit);
    root.append(&line);
    let extra = vbox(6);
    root.append(&extra);
    let detail = label("", &["stat-detail"]);
    detail.set_wrap(true);
    detail.set_visible(false);
    root.append(&detail);
    Stat { root, value, unit, detail, extra }
}

pub fn stat_row(stats: &[&Stat]) -> gtk::Box {
    let row = hbox(22);
    for (i, s) in stats.iter().enumerate() {
        if i > 0 {
            let sep = hbox(0);
            sep.add_css_class("stat-sep");
            row.append(&sep);
        }
        row.append(&s.root);
    }
    row
}

pub fn toggles(options: &[(&str, &str)], active: &str) -> adw::ToggleGroup {
    let g = adw::ToggleGroup::new();
    for &(name, text) in options {
        let t = adw::Toggle::new();
        t.set_name(Some(name));
        t.set_label(Some(text));
        g.add(t);
    }
    g.set_active_name(Some(active));
    g.set_valign(gtk::Align::Center);
    g
}

pub fn active(g: &adw::ToggleGroup) -> String {
    g.active_name().map(|s| s.to_string()).unwrap_or_default()
}

/// A grid table with a header row and hairline separators.
pub struct Table {
    pub grid: gtk::Grid,
    headers: Vec<gtk::Widget>,
    aligns: Vec<f32>,
    row: i32,
}

impl Table {
    /// `cols`: header text and x-alignment (0 = left, 1 = right).
    pub fn new(cols: &[(&str, f32)]) -> Table {
        let heads = cols
            .iter()
            .map(|&(name, align)| {
                let l = label(name, &["table-head"]);
                l.set_xalign(align);
                (l.upcast::<gtk::Widget>(), align)
            })
            .collect();
        Table::with_headers(heads)
    }

    pub fn with_headers(cols: Vec<(gtk::Widget, f32)>) -> Table {
        let grid = gtk::Grid::new();
        grid.set_column_spacing(20);
        let (headers, aligns) = cols.into_iter().unzip();
        let t = Table { grid, headers, aligns, row: 0 };
        t.attach_headers();
        t
    }

    fn attach_headers(&self) {
        for (i, h) in self.headers.iter().enumerate() {
            h.set_margin_bottom(8);
            h.set_valign(gtk::Align::End);
            if i == 0 {
                h.set_hexpand(true);
            }
            self.grid.attach(h, i as i32, 0, 1, 1);
        }
    }

    pub fn clear(&mut self) {
        while let Some(c) = self.grid.first_child() {
            self.grid.remove(&c);
        }
        self.row = 0;
        self.attach_headers();
    }

    /// Appends a row of arbitrary cells.
    pub fn row(&mut self, cells: Vec<gtk::Widget>) {
        self.row += 1;
        let sep = hbox(0);
        sep.add_css_class("table-row-sep");
        self.grid.attach(&sep, 0, self.row * 2 - 1, self.headers.len() as i32, 1);
        for (i, w) in cells.into_iter().enumerate() {
            w.set_margin_top(7);
            w.set_margin_bottom(7);
            w.set_valign(gtk::Align::Center);
            self.grid.attach(&w, i as i32, self.row * 2, 1, 1);
        }
    }

    /// Appends a row: the first cell is a widget, the rest are texts.
    pub fn push(&mut self, first: &impl IsA<gtk::Widget>, cells: &[String], classes: &[&str]) {
        let mut row = vec![first.clone().upcast::<gtk::Widget>()];
        for (i, text) in cells.iter().enumerate() {
            let l = label(text, &["numeric"]);
            for c in classes {
                l.add_css_class(c);
            }
            l.set_xalign(self.aligns.get(i + 1).copied().unwrap_or(1.0));
            row.push(l.upcast());
        }
        self.row(row);
    }
}

/// Wraps page content in a scrolled, width-clamped column.
pub fn page(content: &gtk::Box) -> gtk::ScrolledWindow {
    content.add_css_class("page");
    let clamp = adw::Clamp::builder().maximum_size(1240).tightening_threshold(900).child(content).build();
    gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).child(&clamp).build()
}

/// A caption over a value, optionally clickable, for insight tiles.
pub fn insight(caption: &str, value: &str, detail: &str, on_click: Option<Box<dyn Fn()>>) -> gtk::Widget {
    let b = vbox(2);
    b.append(&label(caption, &["eyebrow"]));
    let v = label(value, &["insight-value"]);
    v.set_ellipsize(gtk::pango::EllipsizeMode::End);
    v.set_tooltip_text(Some(value));
    b.append(&v);
    let d = label(detail, &["stat-detail"]);
    d.set_ellipsize(gtk::pango::EllipsizeMode::End);
    b.append(&d);
    match on_click {
        Some(f) => {
            let btn = gtk::Button::builder().child(&b).hexpand(true).build();
            btn.add_css_class("flat");
            btn.add_css_class("insight");
            btn.connect_clicked(move |_| f());
            btn.upcast()
        }
        None => {
            b.add_css_class("insight");
            b.set_hexpand(true);
            b.upcast()
        }
    }
}
