//! The session dialog: one conversation request by request.

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use super::Ctx;
use super::charts::context::{self, ContextChart};
use super::today::one_line;
use super::widgets;
use crate::analytics;
use crate::format;
use crate::model::{Agent, Call};

/// The command that reopens a session in its agent, where one exists.
fn resume_command(agent: Agent, id: &str) -> Option<String> {
    match agent {
        Agent::Claude => Some(format!("claude --resume {id}")),
        Agent::Codex => Some(format!("codex resume {id}")),
        _ => None,
    }
}

pub fn present(ctx: &Rc<Ctx>, parent: &impl IsA<gtk::Widget>, agent: Agent, id: &str) {
    let ds = ctx.ds();
    // A session includes the subagent conversations it spawned.
    let calls: Vec<Call> =
        ds.calls.iter().filter(|c| c.agent == agent && analytics::root_session(&ds, agent, &c.session) == id).cloned().collect();
    if calls.is_empty() {
        ctx.toast("No requests recorded for this session");
        return;
    }
    let meta = ds.session(agent, id);
    let rows = analytics::sessions(&ds, &calls);
    let Some(row) = rows.first() else { return };
    let title = one_line(&row.title);
    let mut subtitle = vec![agent.label().to_string()];
    if let Some(p) = &row.project {
        subtitle.push(p.clone());
    }
    subtitle.push(analytics::date_of(row.start).format("%a, %b %-d %Y").to_string());

    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(&title, &subtitle.join(" · "))));
    let copy_id = gtk::Button::from_icon_name("edit-copy-symbolic");
    copy_id.set_tooltip_text(Some("Copy session ID"));
    header.pack_end(&copy_id);
    let id_owned = id.to_string();
    let c = Rc::downgrade(ctx);
    copy_id.connect_clicked(move |b| {
        b.clipboard().set_text(&id_owned);
        if let Some(c) = c.upgrade() {
            c.toast("Session ID copied");
        }
    });
    if let Some(cmd) = resume_command(agent, id) {
        let resume = gtk::Button::with_label("Copy Resume Command");
        resume.set_tooltip_text(Some(&cmd));
        header.pack_end(&resume);
        let c = Rc::downgrade(ctx);
        resume.connect_clicked(move |b| {
            b.clipboard().set_text(&cmd);
            if let Some(c) = c.upgrade() {
                c.toast("Resume command copied");
            }
        });
    }

    let body = widgets::vbox(16);
    body.add_css_class("dialog-body");

    let t = &row.totals;
    let sp = analytics::speed(&calls);
    let cost = widgets::stat("Cost");
    let mut cost_detail = Vec::new();
    if t.saved >= 0.01 {
        cost_detail.push(format!("{} saved by caching", format::usd(t.saved)));
    }
    if t.unpriced > 0 {
        cost_detail.push(format!("{} unpriced", format::count(t.unpriced)));
    }
    cost.set(&format::usd(t.usd), "", &cost_detail.join(" · "));
    let requests = widgets::stat("Requests");
    requests.set(&format::count(t.requests), "", &format!("over {}", format::span(row.end - row.start)));
    let tokens = widgets::stat("Tokens");
    tokens.set(
        &format::tokens(t.tokens.total()),
        "",
        &t.cache_hit().map(|h| format!("{} cache hit", format::percent(h))).unwrap_or_default(),
    );
    let context_stat = widgets::stat("Peak context");
    context_stat.set(&format::tokens(row.peak_context), "", &format!("{} output in total", format::tokens(t.tokens.output)));
    let speed = widgets::stat("Output speed");
    let ttft = sp.ttft.map(|q| format!("first token in {}", format::ms(q.p50))).unwrap_or_default();
    match sp.tps {
        Some(q) => speed.set(&format::tps(q.p50), "tok/s", &ttft),
        None => speed.set("—", "", &ttft),
    }
    let stats = widgets::stat_row(&[&cost, &requests, &tokens, &context_stat, &speed]);
    for s in [&cost, &requests, &tokens, &context_stat, &speed] {
        s.value.remove_css_class("stat-value");
        s.value.add_css_class("stat-value-sm");
    }
    body.append(&stats);

    let info = widgets::hbox(8);
    let mut facts = vec![format!("{}–{}", format::clock(row.start), format::clock(row.end))];
    if row.models.len() > 1 {
        facts.push(format!("Models: {}", row.models.join(", ")));
    } else {
        facts.push(row.model.clone());
    }
    if let Some(cwd) = meta.and_then(|m| m.cwd.as_deref()) {
        facts.push(cwd.to_string());
    }
    if row.subagents > 0 {
        facts.push(format!("{} subagent conversations", row.subagents));
    }
    let facts_label = widgets::label(&facts.join("  ·  "), &["stat-detail"]);
    facts_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    facts_label.set_selectable(true);
    info.append(&facts_label);
    body.append(&info);

    let chart_card = widgets::card();
    let (h, _) = widgets::panel_header("Context per request");
    chart_card.append(&h);
    let chart = ContextChart::new(context::Data { calls: calls.clone() });
    chart_card.append(&chart.area);
    body.append(&chart_card);

    // Name subagent conversations in the table so their requests can be told apart.
    let mut names: std::collections::HashMap<String, String> = Default::default();
    if row.subagents > 0 {
        for c in &calls {
            names.entry(c.session.clone()).or_insert_with(|| {
                if c.session == id {
                    "main".to_string()
                } else {
                    ds.session(agent, &c.session)
                        .and_then(|m| m.title.clone())
                        .filter(|t| !t.is_empty())
                        .map(|t| one_line(&t))
                        .unwrap_or_else(|| format!("subagent {}", analytics::short_id(&c.session)))
                }
            });
        }
    }
    let table = requests_table(&calls, names);
    let frame = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .min_content_height(200)
        .child(&table)
        .build();
    frame.add_css_class("card");
    frame.add_css_class("requests-frame");
    body.append(&frame);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&body));

    let dialog = adw::Dialog::builder().title(&title).content_width(1040).content_height(780).child(&toolbar).build();
    dialog.present(Some(parent));
}

/// Title, x-alignment, whether it takes spare width, and the cell text.
type Column = (&'static str, f32, bool, fn(&Call) -> String);

fn requests_table(calls: &[Call], names: std::collections::HashMap<String, String>) -> gtk::ColumnView {
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let items: Vec<glib::BoxedAnyObject> = calls.iter().cloned().map(glib::BoxedAnyObject::new).collect();
    store.extend_from_slice(&items);
    let view = gtk::ColumnView::new(Some(gtk::NoSelection::new(Some(store))));
    view.add_css_class("data-table");
    view.set_reorderable(false);

    type Cell = Rc<dyn Fn(&Call) -> String>;
    let mut columns: Vec<(&str, f32, bool, Cell)> =
        vec![("Time", 0.0, false, Rc::new(|c: &Call| analytics::local(c.ts).format("%H:%M:%S").to_string()))];
    if !names.is_empty() {
        columns.push(("Conversation", 0.0, true, Rc::new(move |c: &Call| names.get(&c.session).cloned().unwrap_or_default())));
    }
    let rest: [Column; 9] = [
        ("Model", 0.0, true, |c| match &c.effort {
            Some(e) => format!("{} · {e}", c.model),
            None => c.model.clone(),
        }),
        ("Context", 1.0, false, |c| format::tokens(c.tokens.prompt())),
        ("Cached", 1.0, false, |c| {
            let p = c.tokens.prompt();
            if p == 0 { "—".into() } else { format::percent(c.tokens.cache_read as f64 / p as f64) }
        }),
        ("Written", 1.0, false, |c| format::tokens(c.tokens.cache_write + c.tokens.cache_write_1h)),
        ("New", 1.0, false, |c| format::tokens(c.tokens.input)),
        ("Output", 1.0, false, |c| format::tokens(c.tokens.output)),
        ("First token", 1.0, false, |c| c.ttft_ms.map(|v| format::ms(v as f64)).unwrap_or_else(|| "—".into())),
        ("tok/s", 1.0, false, |c| c.tps().map(format::tps).unwrap_or_else(|| "—".into())),
        ("Cost", 1.0, false, |c| {
            if c.calls > 1 { format!("{} · {} calls", format::usd_exact(c.usd), c.calls) } else { format::usd_exact(c.usd) }
        }),
    ];
    columns.extend(rest.into_iter().map(|(t, x, e, f)| (t, x, e, Rc::new(f) as Cell)));
    for (title, xalign, expand, cell) in columns {
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(move |_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
            let l = gtk::Label::new(None);
            l.set_xalign(xalign);
            l.add_css_class("numeric");
            if expand {
                l.set_ellipsize(gtk::pango::EllipsizeMode::End);
            }
            item.set_child(Some(&l));
        });
        factory.connect_bind(move |_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
            let (Some(obj), Some(l)) = (item.item().and_downcast::<glib::BoxedAnyObject>(), item.child().and_downcast::<gtk::Label>())
            else {
                return;
            };
            let c = obj.borrow::<Call>();
            let text = cell(&c);
            if expand {
                l.set_tooltip_text(Some(&text));
            }
            l.set_label(&text);
        });
        let col = gtk::ColumnViewColumn::new(Some(title), Some(factory));
        col.set_expand(expand);
        col.set_resizable(true);
        view.append_column(&col);
    }
    view
}
