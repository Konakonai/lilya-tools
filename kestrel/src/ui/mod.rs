//! GTK4 / libadwaita interface.
//!
//! A worker thread owns the [`Engine`]: it scans once, then follows the log
//! folders with inotify and sends a fresh [`Dataset`] whenever something
//! changes. The UI thread only renders snapshots.

mod charts;
mod history;
mod models;
mod prices;
mod session;
mod sessions;
mod theme;
mod today;
mod widgets;
mod window;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{gdk, glib};
use notify::Watcher;

use crate::Paths;
use crate::ingest::{Dataset, Engine};
use crate::model::Agent;
use crate::pricing::Catalog;
use crate::sources;

pub const APP_ID: &str = "io.github.konakonai.Kestrel";

pub enum ToWorker {
    Files(Vec<PathBuf>),
    Rescan,
    RefreshPrices,
    Rebuild,
}

pub enum ToUi {
    Data(Dataset),
    Busy(Option<String>),
    Toast(String),
}

/// State shared by all pages, owned by the UI thread.
pub struct Ctx {
    pub ds: RefCell<Rc<Dataset>>,
    pub catalog: Catalog,
    pub worker: mpsc::Sender<ToWorker>,
    pub nav: RefCell<Option<Rc<window::Window>>>,
}

impl Ctx {
    pub fn ds(&self) -> Rc<Dataset> {
        self.ds.borrow().clone()
    }

    pub fn send(&self, m: ToWorker) {
        let _ = self.worker.send(m);
    }

    pub fn window(&self) -> Option<Rc<window::Window>> {
        self.nav.borrow().clone()
    }

    pub fn toast(&self, msg: &str) {
        if let Some(w) = self.window() {
            w.toasts.add_toast(adw::Toast::new(msg));
        }
    }

    pub fn show_day(&self, d: chrono::NaiveDate) {
        if let Some(w) = self.window() {
            w.today.set_day(d);
            w.stack.set_visible_child_name("today");
        }
    }

    pub fn open_session(self: &Rc<Self>, agent: Agent, id: &str) {
        if let Some(w) = self.window() {
            session::present(self, &w.win, agent, id);
        }
    }
}

pub fn run(paths: Paths, catalog: Catalog, magpie: bool) -> std::process::ExitCode {
    // Single instance: launching again raises the existing window instead of
    // starting a second log scanner.
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_startup(|_| load_css());
    app.connect_activate(move |app| match app.active_window() {
        Some(w) => w.present(),
        None => activate(app, &paths, catalog.clone(), magpie),
    });
    // Arguments were already parsed by clap.
    let code = app.run_with_args::<&str>(&[]);
    code.into()
}

fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(include_str!("style.css"));
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }
}

fn activate(app: &adw::Application, paths: &Paths, catalog: Catalog, magpie: bool) {
    let (to_worker, worker_rx) = mpsc::channel::<ToWorker>();
    let (to_ui, ui_rx) = async_channel::unbounded::<ToUi>();

    let ctx = Rc::new(Ctx {
        ds: RefCell::new(Rc::new(Dataset::default())),
        catalog: catalog.clone(),
        worker: to_worker.clone(),
        nav: RefCell::new(None),
    });
    let win = window::Window::new(app, &ctx);
    *ctx.nav.borrow_mut() = Some(win.clone());
    win.win.present();

    spawn_worker(paths, catalog, magpie, to_worker, worker_rx, to_ui);

    let ctx2 = ctx.clone();
    glib::spawn_future_local(async move {
        let mut first = true;
        while let Ok(msg) = ui_rx.recv().await {
            let Some(w) = ctx2.window() else { break };
            match msg {
                ToUi::Data(ds) => {
                    *ctx2.ds.borrow_mut() = Rc::new(ds);
                    w.refresh(&ctx2);
                    if std::mem::take(&mut first) {
                        open_from_env(&ctx2);
                    }
                }
                ToUi::Busy(label) => w.set_busy(label.as_deref()),
                ToUi::Toast(t) => ctx2.toast(&t),
            }
        }
    });

    // Keep relative times and the "now" marker moving even when logs are quiet.
    let ctx3 = ctx.clone();
    glib::timeout_add_seconds_local(30, move || {
        if let Some(w) = ctx3.window() {
            w.tick(&ctx3);
        }
        glib::ControlFlow::Continue
    });
}

/// `KESTREL_OPEN` jumps to a view once data is loaded, for screenshots and
/// testing: a page name, `day:YYYY-MM-DD`, `session:<agent>:<id>` or `prices`.
fn open_from_env(ctx: &Rc<Ctx>) {
    let Ok(target) = std::env::var("KESTREL_OPEN") else { return };
    let Some(w) = ctx.window() else { return };
    match target.split_once(':') {
        Some(("day", d)) => {
            if let Ok(d) = chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d") {
                ctx.show_day(d);
            }
        }
        Some(("session", rest)) => {
            if let Some((agent, id)) = rest.split_once(':').and_then(|(a, id)| Some((Agent::from_slug(a)?, id))) {
                ctx.open_session(agent, id);
            }
        }
        _ if target == "prices" => prices::present(ctx, &w.win),
        _ => w.stack.set_visible_child_name(&target),
    }
}

fn spawn_worker(
    paths: &Paths,
    catalog: Catalog,
    magpie: bool,
    to_self: mpsc::Sender<ToWorker>,
    rx: mpsc::Receiver<ToWorker>,
    ui: async_channel::Sender<ToUi>,
) {
    let home = paths.home.clone();
    let cache = paths.cache.clone();
    std::thread::Builder::new()
        .name("kestrel-ingest".into())
        .spawn(move || {
            let send = |m: ToUi| {
                let _ = ui.send_blocking(m);
            };
            let mut engine = Engine::open(&home, &cache, magpie);
            send(ToUi::Busy(Some("Reading agent logs…".into())));
            engine.full_scan();
            send(ToUi::Data(engine.build(&catalog)));
            send(ToUi::Busy(None));

            let stale = |c: &Catalog| c.is_stale(chrono::Utc::now().timestamp());
            if stale(&catalog) {
                send(ToUi::Busy(Some("Updating prices…".into())));
                match catalog.refresh() {
                    Ok(_) => send(ToUi::Data(engine.build(&catalog))),
                    Err(e) => send(ToUi::Toast(format!("Could not update prices: {e}"))),
                }
                send(ToUi::Busy(None));
            }

            // Follow the log folders. The watcher lives as long as this thread.
            let fs_tx = to_self.clone();
            let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                if let Ok(ev) = res
                    && !ev.paths.is_empty()
                {
                    let _ = fs_tx.send(ToWorker::Files(ev.paths));
                }
            })
            .ok();
            if let Some(w) = watcher.as_mut() {
                for (root, _) in sources::roots(&home) {
                    if root.is_dir() {
                        let _ = w.watch(&root, notify::RecursiveMode::Recursive);
                    }
                }
            }

            loop {
                match rx.recv_timeout(Duration::from_secs(600)) {
                    Ok(ToWorker::Files(mut paths)) => {
                        // Agents write in bursts; settle before re-reading.
                        let until = Instant::now() + Duration::from_millis(600);
                        while let Some(left) = until.checked_duration_since(Instant::now()) {
                            match rx.recv_timeout(left) {
                                Ok(ToWorker::Files(more)) => paths.extend(more),
                                Ok(other) => {
                                    let _ = to_self.send(other);
                                    break;
                                }
                                Err(_) => break,
                            }
                        }
                        paths.sort();
                        paths.dedup();
                        if engine.update_paths(&paths) {
                            send(ToUi::Data(engine.build(&catalog)));
                        }
                    }
                    Ok(ToWorker::Rescan) => {
                        send(ToUi::Busy(Some("Rescanning…".into())));
                        engine.full_scan();
                        send(ToUi::Data(engine.build(&catalog)));
                        send(ToUi::Busy(None));
                    }
                    Ok(ToWorker::RefreshPrices) => {
                        send(ToUi::Busy(Some("Updating prices…".into())));
                        match catalog.refresh() {
                            Ok(n) => {
                                send(ToUi::Data(engine.build(&catalog)));
                                send(ToUi::Toast(format!("Prices updated · {n} models")));
                            }
                            Err(e) => send(ToUi::Toast(format!("Could not update prices: {e}"))),
                        }
                        send(ToUi::Busy(None));
                    }
                    Ok(ToWorker::Rebuild) => {
                        catalog.reload_overrides();
                        send(ToUi::Data(engine.build(&catalog)));
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if stale(&catalog) && catalog.refresh().is_ok() {
                            send(ToUi::Data(engine.build(&catalog)));
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            drop(watcher);
        })
        .expect("spawn ingest thread");
}
