//! The main window: header with live status, four pages, banner and toasts.

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;

use super::today::now_ms;
use super::{APP_ID, Ctx, ToWorker, history, models, prices, sessions, today, widgets};
use crate::format;

const PAGES: [&str; 4] = ["today", "history", "models", "sessions"];

pub struct Window {
    pub win: adw::ApplicationWindow,
    pub toasts: adw::ToastOverlay,
    pub stack: adw::ViewStack,
    pub today: Rc<today::TodayPage>,
    history: Rc<history::HistoryPage>,
    models: Rc<models::ModelsPage>,
    sessions: Rc<sessions::SessionsPage>,
    banner: adw::Banner,
    /// Unpriced models the user already saw in the banner.
    banner_seen: RefCell<BTreeSet<String>>,
    live: gtk::Box,
    live_dot: gtk::Box,
    live_label: gtk::Label,
    busy: gtk::Box,
    busy_label: gtk::Label,
    /// Pages whose data changed while they were hidden.
    stale: Cell<[bool; 4]>,
}

impl Window {
    pub fn new(app: &adw::Application, ctx: &Rc<Ctx>) -> Rc<Window> {
        let today = today::TodayPage::new(ctx);
        let history = history::HistoryPage::new(ctx);
        let models = models::ModelsPage::new(ctx);
        let sessions = sessions::SessionsPage::new(ctx);

        let stack = adw::ViewStack::new();
        stack.set_vexpand(true);
        stack.add_titled_with_icon(&today.root, Some("today"), "Today", "document-open-recent-symbolic");
        stack.add_titled_with_icon(&history.root, Some("history"), "History", "x-office-calendar-symbolic");
        stack.add_titled_with_icon(&models.root, Some("models"), "Models", "power-profile-performance-symbolic");
        stack.add_titled_with_icon(&sessions.root, Some("sessions"), "Sessions", "view-list-symbolic");

        let switcher = adw::ViewSwitcher::builder().stack(&stack).policy(adw::ViewSwitcherPolicy::Wide).build();
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&switcher));

        let live = widgets::hbox(7);
        live.add_css_class("live-pill");
        live.set_valign(gtk::Align::Center);
        let live_dot = widgets::hbox(0);
        live_dot.add_css_class("live-dot");
        live_dot.set_valign(gtk::Align::Center);
        let live_label = widgets::label("Idle", &[]);
        live.append(&live_dot);
        live.append(&live_label);
        header.pack_start(&live);

        let busy = widgets::hbox(8);
        busy.set_valign(gtk::Align::Center);
        busy.append(&adw::Spinner::new());
        let busy_label = widgets::label("", &["dim"]);
        busy.append(&busy_label);
        busy.set_visible(false);
        header.pack_start(&busy);

        let menu = gio::Menu::new();
        let section = gio::Menu::new();
        section.append(Some("Rescan Logs"), Some("win.rescan"));
        section.append(Some("Update Prices"), Some("win.update-prices"));
        section.append(Some("Prices…"), Some("win.prices"));
        menu.append_section(None, &section);
        let section = gio::Menu::new();
        section.append(Some("Keyboard Shortcuts"), Some("win.shortcuts"));
        section.append(Some("About Kestrel"), Some("win.about"));
        menu.append_section(None, &section);
        let menu_button =
            gtk::MenuButton::builder().icon_name("open-menu-symbolic").menu_model(&menu).primary(true).tooltip_text("Main Menu").build();
        header.pack_end(&menu_button);

        let banner = adw::Banner::new("");
        banner.set_button_label(Some("Set Prices"));

        let content = widgets::vbox(0);
        content.append(&banner);
        content.append(&stack);
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&content));

        let switcher_bar = adw::ViewSwitcherBar::builder().stack(&stack).build();
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&toasts));
        toolbar.add_bottom_bar(&switcher_bar);

        let win = adw::ApplicationWindow::builder()
            .application(app)
            .title("Kestrel")
            .icon_name(APP_ID)
            .default_width(1280)
            .default_height(880)
            .width_request(380)
            .height_request(420)
            .content(&toolbar)
            .build();

        let narrow = adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 760sp").expect("valid breakpoint"));
        narrow.add_setter(&switcher_bar, "reveal", Some(&true.to_value()));
        narrow.add_setter(&header, "title-widget", Some(&None::<gtk::Widget>.to_value()));
        win.add_breakpoint(narrow);

        let w = Rc::new(Window {
            win,
            toasts,
            stack,
            today,
            history,
            models,
            sessions,
            banner,
            banner_seen: RefCell::default(),
            live,
            live_dot,
            live_label,
            busy,
            busy_label,
            stale: Cell::new([false; 4]),
        });
        w.install_actions(app, ctx);

        let weak = Rc::downgrade(&w);
        w.stack.connect_visible_child_name_notify(move |_| {
            if let Some(w) = weak.upgrade() {
                w.refresh_visible(false);
            }
        });
        let (weak, c) = (Rc::downgrade(&w), Rc::downgrade(ctx));
        w.banner.connect_button_clicked(move |b| {
            if let (Some(w), Some(c)) = (weak.upgrade(), c.upgrade()) {
                w.banner_seen.borrow_mut().extend(prices::unpriced(&c).into_iter().map(|m| m.0));
                b.set_revealed(false);
                prices::present(&c, &w.win);
            }
        });
        w
    }

    fn install_actions(self: &Rc<Self>, app: &adw::Application, ctx: &Rc<Ctx>) {
        let add = |name: &str, f: Box<dyn Fn()>| {
            let a = gio::SimpleAction::new(name, None);
            a.connect_activate(move |_, _| f());
            self.win.add_action(&a);
        };
        let c = Rc::downgrade(ctx);
        add(
            "rescan",
            Box::new(move || {
                if let Some(c) = c.upgrade() {
                    c.send(ToWorker::Rescan);
                }
            }),
        );
        let c = Rc::downgrade(ctx);
        add(
            "update-prices",
            Box::new(move || {
                if let Some(c) = c.upgrade() {
                    c.send(ToWorker::RefreshPrices);
                }
            }),
        );
        let c = Rc::downgrade(ctx);
        add(
            "prices",
            Box::new(move || {
                if let Some(c) = c.upgrade()
                    && let Some(w) = c.window()
                {
                    prices::present(&c, &w.win);
                }
            }),
        );
        let w = Rc::downgrade(self);
        add(
            "about",
            Box::new(move || {
                if let Some(w) = w.upgrade() {
                    about().present(Some(&w.win));
                }
            }),
        );
        let w = Rc::downgrade(self);
        add(
            "shortcuts",
            Box::new(move || {
                if let Some(w) = w.upgrade() {
                    shortcuts().present(Some(&w.win));
                }
            }),
        );
        for (name, days) in [("prev-day", -1), ("next-day", 1)] {
            let w = Rc::downgrade(self);
            add(
                name,
                Box::new(move || {
                    if let Some(w) = w.upgrade() {
                        w.stack.set_visible_child_name("today");
                        w.today.step(days);
                    }
                }),
            );
        }
        let w = Rc::downgrade(self);
        add(
            "go-today",
            Box::new(move || {
                if let Some(w) = w.upgrade() {
                    w.stack.set_visible_child_name("today");
                    w.today.set_day(crate::analytics::today());
                }
            }),
        );
        for (i, page) in PAGES.iter().enumerate() {
            let w = Rc::downgrade(self);
            add(
                &format!("page-{}", i + 1),
                Box::new(move || {
                    if let Some(w) = w.upgrade() {
                        w.stack.set_visible_child_name(page);
                    }
                }),
            );
            app.set_accels_for_action(&format!("win.page-{}", i + 1), &[&format!("<Alt>{}", i + 1)]);
        }
        let w = Rc::downgrade(self);
        add(
            "search",
            Box::new(move || {
                if let Some(w) = w.upgrade() {
                    w.stack.set_visible_child_name("sessions");
                    w.sessions.search.grab_focus();
                }
            }),
        );

        let quit = gio::SimpleAction::new("quit", None);
        let a = app.downgrade();
        quit.connect_activate(move |_, _| {
            if let Some(a) = a.upgrade() {
                a.quit();
            }
        });
        app.add_action(&quit);

        app.set_accels_for_action("win.rescan", &["<Primary>r"]);
        app.set_accels_for_action("win.prev-day", &["<Alt>Left"]);
        app.set_accels_for_action("win.next-day", &["<Alt>Right"]);
        app.set_accels_for_action("win.go-today", &["<Alt>Home"]);
        app.set_accels_for_action("win.search", &["<Primary>f"]);
        app.set_accels_for_action("win.shortcuts", &["<Primary>question"]);
        app.set_accels_for_action("window.close", &["<Primary>w"]);
        app.set_accels_for_action("app.quit", &["<Primary>q"]);
    }

    fn page_index(&self) -> usize {
        let name = self.stack.visible_child_name().map(|s| s.to_string()).unwrap_or_default();
        PAGES.iter().position(|p| *p == name).unwrap_or(0)
    }

    fn refresh_page(&self, i: usize) {
        match i {
            0 => self.today.refresh(),
            1 => self.history.refresh(),
            2 => self.models.refresh(),
            _ => self.sessions.refresh(),
        }
    }

    /// Refreshes the visible page now; with `all`, marks the others stale.
    fn refresh_visible(&self, all: bool) {
        let i = self.page_index();
        let mut stale = self.stale.get();
        if all {
            stale = [true; 4];
        }
        if stale[i] {
            self.refresh_page(i);
            stale[i] = false;
        }
        self.stale.set(stale);
    }

    /// New data arrived.
    pub fn refresh(&self, ctx: &Ctx) {
        self.refresh_visible(true);
        self.update_live(ctx);
        self.update_banner(ctx);
    }

    /// Every 30 seconds.
    pub fn tick(&self, ctx: &Ctx) {
        if self.page_index() == 0 {
            self.today.tick();
        } else {
            let mut s = self.stale.get();
            s[0] = true;
            self.stale.set(s);
        }
        self.update_live(ctx);
    }

    pub fn set_busy(&self, label: Option<&str>) {
        self.busy.set_visible(label.is_some());
        self.live.set_visible(label.is_none());
        self.busy_label.set_label(label.unwrap_or_default());
    }

    fn update_live(&self, ctx: &Ctx) {
        let ds = ctx.ds();
        let now = now_ms();
        let recent = ds.between(now - 3 * 60_000, now + 60_000);
        let mut sessions: Vec<(crate::model::Agent, &str)> = recent.iter().map(|c| (c.agent, c.session.as_str())).collect();
        sessions.sort();
        sessions.dedup();
        if sessions.is_empty() {
            self.live_dot.remove_css_class("active");
            self.live_label.set_label("Idle");
            let tip = match ds.calls.last() {
                Some(c) => format!("Last request {}", format::ago(c.ts, now)),
                None => "No requests yet".into(),
            };
            self.live.set_tooltip_text(Some(&tip));
        } else {
            self.live_dot.add_css_class("active");
            self.live_label.set_label(&if sessions.len() == 1 { "Live".to_string() } else { format!("Live · {}", sessions.len()) });
            let names: Vec<String> = sessions
                .iter()
                .map(|(a, id)| {
                    let title = ds.session(*a, id).and_then(|m| m.title.clone()).unwrap_or_else(|| crate::analytics::short_id(id));
                    format!("{} · {}", a.label(), today::one_line(&title))
                })
                .collect();
            self.live.set_tooltip_text(Some(&format!("Active in the last 3 minutes:\n{}", names.join("\n"))));
        }
    }

    fn update_banner(&self, ctx: &Ctx) {
        let missing = prices::unpriced(ctx);
        let fresh: Vec<&(String, crate::model::Agent, u64, u64)> =
            missing.iter().filter(|m| !self.banner_seen.borrow().contains(&m.0)).collect();
        if fresh.is_empty() {
            self.banner.set_revealed(false);
            return;
        }
        let requests: u64 = fresh.iter().map(|m| m.2).sum();
        let title = if fresh.len() == 1 {
            format!("{} has no price, so {} requests count as $0", fresh[0].0, format::count(requests))
        } else {
            format!("{} models have no price, so {} requests count as $0", fresh.len(), format::count(requests))
        };
        self.banner.set_title(&title);
        self.banner.set_revealed(true);
    }
}

fn about() -> adw::AboutDialog {
    adw::AboutDialog::builder()
        .application_name("Kestrel")
        .application_icon(APP_ID)
        .version(env!("CARGO_PKG_VERSION"))
        .developer_name("Konakonai")
        .comments("Watches your AI coding agents: what they cost, how fast they answer, and where the tokens go. Reads local logs from Claude Code, Codex, Antigravity, Grok, Pi and Magpie; nothing leaves your machine except price list downloads.")
        .website("https://github.com/Konakonai/lilya-tools")
        .build()
}

fn shortcuts() -> adw::ShortcutsDialog {
    let dialog = adw::ShortcutsDialog::new();
    let nav = adw::ShortcutsSection::new(Some("Navigation"));
    for (title, accel) in [
        ("Today", "<Alt>1"),
        ("History", "<Alt>2"),
        ("Models", "<Alt>3"),
        ("Sessions", "<Alt>4"),
        ("Previous day", "<Alt>Left"),
        ("Next day", "<Alt>Right"),
        ("Back to today", "<Alt>Home"),
        ("Search sessions", "<Primary>f"),
    ] {
        nav.add(adw::ShortcutsItem::new(title, accel));
    }
    dialog.add(nav);
    let general = adw::ShortcutsSection::new(Some("General"));
    for (title, accel) in
        [("Rescan logs", "<Primary>r"), ("Keyboard shortcuts", "<Primary>question"), ("Close window", "<Primary>w"), ("Quit", "<Primary>q")]
    {
        general.add(adw::ShortcutsItem::new(title, accel));
    }
    dialog.add(general);
    dialog
}
