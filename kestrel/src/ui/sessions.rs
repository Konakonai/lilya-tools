//! Sessions: every conversation, searchable, newest first.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use chrono::Duration;
use gtk::{gio, glib};

use super::Ctx;
use super::today::{now_ms, one_line};
use super::widgets;
use crate::analytics::{self, SessionRow};
use crate::format;
use crate::model::Agent;

pub struct SessionsPage {
    pub root: gtk::Box,
    ctx: Weak<Ctx>,
    pub search: gtk::SearchEntry,
    agent: gtk::DropDown,
    range: adw::ToggleGroup,
    summary: gtk::Label,
    store: gio::ListStore,
    stack: gtk::Stack,
    rows: RefCell<Vec<Rc<SessionRow>>>,
}

impl SessionsPage {
    pub fn new(ctx: &Rc<Ctx>) -> Rc<SessionsPage> {
        let root = widgets::vbox(0);

        let bar = widgets::vbox(14);
        bar.add_css_class("page");
        bar.set_margin_bottom(0);
        let header = widgets::hbox(12);
        let titles = widgets::vbox(0);
        titles.append(&widgets::label("Sessions", &["day-title"]));
        let summary = widgets::label("", &["day-subtitle"]);
        titles.append(&summary);
        header.append(&titles);
        header.append(&widgets::spacer());
        let range = widgets::toggles(&[("7d", "7 days"), ("30d", "30 days"), ("90d", "90 days"), ("all", "All")], "30d");
        header.append(&range);
        bar.append(&header);
        let filters = widgets::hbox(10);
        let search = gtk::SearchEntry::builder().placeholder_text("Search titles, projects, models").hexpand(true).build();
        let mut names = vec!["All agents"];
        names.extend(Agent::ALL.iter().map(|a| a.label()));
        let agent = gtk::DropDown::from_strings(&names);
        filters.append(&search);
        filters.append(&agent);
        bar.append(&filters);
        let bar_clamp = adw::Clamp::builder().maximum_size(1240).tightening_threshold(900).child(&bar).build();
        root.append(&bar_clamp);

        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_bind(|_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
            let Some(obj) = item.item().and_downcast::<glib::BoxedAnyObject>() else { return };
            let row = obj.borrow::<Rc<SessionRow>>();
            item.set_child(Some(&row_widget(&row)));
        });
        let list = gtk::ListView::new(Some(gtk::NoSelection::new(Some(store.clone()))), Some(factory));
        list.set_single_click_activate(true);
        list.add_css_class("session-list");
        let scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).child(&list).build();
        let list_clamp = adw::Clamp::builder().maximum_size(1240).tightening_threshold(900).child(&scroller).vexpand(true).build();
        list_clamp.add_css_class("session-list-clamp");

        let empty = adw::StatusPage::builder()
            .icon_name("system-search-symbolic")
            .title("No sessions")
            .description("Nothing matches these filters.")
            .build();
        let stack = gtk::Stack::new();
        stack.add_named(&list_clamp, Some("list"));
        stack.add_named(&empty, Some("empty"));
        root.append(&stack);

        let page = Rc::new(SessionsPage {
            root,
            ctx: Rc::downgrade(ctx),
            search: search.clone(),
            agent: agent.clone(),
            range: range.clone(),
            summary,
            store: store.clone(),
            stack,
            rows: RefCell::new(Vec::new()),
        });

        let c = Rc::downgrade(ctx);
        list.connect_activate(move |_, pos| {
            let Some(obj) = store.item(pos).and_downcast::<glib::BoxedAnyObject>() else { return };
            let (agent, id) = {
                let r = obj.borrow::<Rc<SessionRow>>();
                (r.agent, r.id.clone())
            };
            if let Some(c) = c.upgrade() {
                c.open_session(agent, &id);
            }
        });
        let w = Rc::downgrade(&page);
        search.connect_search_changed(move |_| {
            if let Some(p) = w.upgrade() {
                p.apply_filter();
            }
        });
        let w = Rc::downgrade(&page);
        agent.connect_selected_notify(move |_| {
            if let Some(p) = w.upgrade() {
                p.apply_filter();
            }
        });
        let w = Rc::downgrade(&page);
        range.connect_active_name_notify(move |_| {
            if let Some(p) = w.upgrade() {
                p.refresh();
            }
        });
        page
    }

    pub fn refresh(&self) {
        let Some(ctx) = self.ctx.upgrade() else { return };
        let ds = ctx.ds();
        let today = analytics::today();
        let (_, end) = analytics::day_bounds(today);
        let start = match widgets::active(&self.range).as_str() {
            "7d" => analytics::day_bounds(today - Duration::days(6)).0,
            "90d" => analytics::day_bounds(today - Duration::days(89)).0,
            "all" => i64::MIN,
            _ => analytics::day_bounds(today - Duration::days(29)).0,
        };
        let mut rows = analytics::sessions(&ds, ds.between(start, end));
        rows.sort_by_key(|r| std::cmp::Reverse(r.end));
        *self.rows.borrow_mut() = rows.into_iter().map(Rc::new).collect();
        self.apply_filter();
    }

    fn apply_filter(&self) {
        let q = self.search.text().to_lowercase();
        let agent = match self.agent.selected() {
            0 => None,
            i => Agent::ALL.get(i as usize - 1).copied(),
        };
        let rows = self.rows.borrow();
        let hits: Vec<glib::BoxedAnyObject> = rows
            .iter()
            .filter(|r| agent.is_none_or(|a| r.agent == a))
            .filter(|r| {
                q.is_empty()
                    || r.title.to_lowercase().contains(&q)
                    || r.project.as_deref().is_some_and(|p| p.to_lowercase().contains(&q))
                    || r.models.iter().any(|m| m.to_lowercase().contains(&q))
                    || r.id.starts_with(&q)
            })
            .map(|r| glib::BoxedAnyObject::new(r.clone()))
            .collect();
        let usd: f64 = hits.iter().map(|o| o.borrow::<Rc<SessionRow>>().totals.usd).sum();
        self.summary.set_label(&format!("{} sessions · {}", format::count(hits.len() as u64), format::usd(usd)));
        self.store.remove_all();
        self.store.extend_from_slice(&hits);
        self.stack.set_visible_child_name(if hits.is_empty() { "empty" } else { "list" });
    }
}

fn row_widget(r: &SessionRow) -> gtk::Box {
    let row = widgets::hbox(14);
    row.add_css_class("session-row");
    row.append(&widgets::agent_dot(r.agent));

    let mid = widgets::vbox(2);
    mid.set_hexpand(true);
    let title = widgets::label(&one_line(&r.title), &["session-title"]);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    mid.append(&title);
    let mut sub = vec![r.agent.label().to_string()];
    if let Some(p) = &r.project
        && *p != r.title
    {
        sub.push(p.clone());
    }
    if !r.model.is_empty() {
        let extra = r.models.len().saturating_sub(1);
        sub.push(if extra > 0 { format!("{} +{extra}", r.model) } else { r.model.clone() });
    }
    sub.push(format!("{} requests", format::count(r.totals.requests)));
    match r.subagents {
        0 => {}
        1 => sub.push("1 subagent".into()),
        n => sub.push(format!("{n} subagents")),
    }
    let subtitle = widgets::label(&sub.join(" · "), &["stat-detail"]);
    subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);
    mid.append(&subtitle);
    row.append(&mid);

    let right = widgets::vbox(2);
    let cost = widgets::label(&format::usd(r.totals.usd), &["session-title", "numeric"]);
    cost.set_xalign(1.0);
    right.append(&cost);
    let when = widgets::label(&format!("{} · {}", when(r.end), format::span(r.end - r.start)), &["stat-detail"]);
    when.set_xalign(1.0);
    right.append(&when);
    row.append(&right);
    row
}

fn when(ms: i64) -> String {
    let d = analytics::date_of(ms);
    let today = analytics::today();
    let days = (today - d).num_days();
    if days == 0 {
        format!("Today {}", format::clock(ms))
    } else if days == 1 {
        format!("Yesterday {}", format::clock(ms))
    } else if days < 7 {
        d.format("%a ").to_string() + &format::clock(ms)
    } else if now_ms() - ms < 330 * 86_400_000 {
        format::date_short(d)
    } else {
        d.format("%b %-d, %Y").to_string()
    }
}
