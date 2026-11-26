//! The prices dialog: catalog status and fixes for models without a price.

use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;

use super::today::now_ms;
use super::{Ctx, ToWorker};
use crate::format;
use crate::model::Agent;
use crate::pricing::Match;

const TEMPLATE: &str = "# Kestrel price overrides.\n\
# [alias] maps a model id to a catalog id; [model.\"id\"] sets USD per million tokens.\n\
#\n\
# [alias]\n\
# \"gemini-pro-default\" = \"gemini-3-pro-preview\"\n\
#\n\
# [model.\"my-local-model\"]\n\
# input = 0.0\n\
# output = 0.0\n";

/// Models that had requests but no price, most used first.
pub fn unpriced(ctx: &Ctx) -> Vec<(String, Agent, u64, u64)> {
    let ds = ctx.ds();
    let mut map: HashMap<&str, (Agent, u64, u64)> = HashMap::new();
    for c in ds.calls.iter().filter(|c| c.priced == Match::None) {
        let e = map.entry(c.model.as_str()).or_insert((c.agent, 0, 0));
        e.1 += c.calls as u64;
        e.2 += c.tokens.total();
    }
    let mut v: Vec<_> = map.into_iter().map(|(m, (a, n, t))| (m.to_string(), a, n, t)).collect();
    v.sort_by_key(|m| std::cmp::Reverse(m.2));
    v
}

pub fn present(ctx: &Rc<Ctx>, parent: &impl IsA<gtk::Widget>) {
    let dialog = adw::PreferencesDialog::new();
    dialog.set_title("Prices");
    dialog.set_search_enabled(false);
    let page = adw::PreferencesPage::new();
    dialog.add(&page);

    let catalog = adw::PreferencesGroup::builder()
        .title("Price list")
        .description(
            "Per-token prices come from models.dev, with OpenRouter filling gaps. First-party providers win when both list a model.",
        )
        .build();
    let (count, fetched, error) = ctx.catalog.status();
    let mut sub = if fetched > 0 {
        format!("{} models · updated {}", format::count(count as u64), format::ago(fetched * 1000, now_ms()))
    } else {
        "Not downloaded yet".to_string()
    };
    if let Some(e) = error {
        sub.push_str(&format!("\nLast update failed: {e}"));
    }
    let status = adw::ActionRow::builder().title("Catalog").subtitle(&sub).build();
    let update = gtk::Button::builder().label("Update").valign(gtk::Align::Center).build();
    let c = Rc::downgrade(ctx);
    update.connect_clicked(move |b| {
        if let Some(c) = c.upgrade() {
            c.send(ToWorker::RefreshPrices);
            b.set_sensitive(false);
            b.set_label("Updating…");
        }
    });
    status.add_suffix(&update);
    catalog.add(&status);

    let path = ctx.catalog.override_path().to_path_buf();
    let file_row = adw::ActionRow::builder().title("Overrides").subtitle(path.display().to_string()).subtitle_selectable(true).build();
    let open = gtk::Button::builder().label("Open").valign(gtk::Align::Center).build();
    let c = Rc::downgrade(ctx);
    open.connect_clicked(move |b| {
        if !path.exists() {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Err(e) = std::fs::write(&path, TEMPLATE) {
                if let Some(c) = c.upgrade() {
                    c.toast(&format!("Could not create {}: {e}", path.display()));
                }
                return;
            }
        }
        let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(&path)));
        let root = b.root().and_downcast::<gtk::Window>();
        let c = c.clone();
        launcher.launch(root.as_ref(), None::<&gio::Cancellable>, move |res| {
            if let (Err(e), Some(c)) = (res, c.upgrade()) {
                c.toast(&format!("Could not open the file: {e}"));
            }
        });
    });
    file_row.add_suffix(&open);
    let reload =
        gtk::Button::builder().icon_name("view-refresh-symbolic").tooltip_text("Reload after editing").valign(gtk::Align::Center).build();
    reload.add_css_class("flat");
    let c = Rc::downgrade(ctx);
    reload.connect_clicked(move |_| {
        if let Some(c) = c.upgrade() {
            c.send(ToWorker::Rebuild);
            c.toast("Overrides reloaded");
        }
    });
    file_row.add_suffix(&reload);
    catalog.add(&file_row);
    page.add(&catalog);

    let missing = unpriced(ctx);
    let group = adw::PreferencesGroup::builder().title("Unpriced models").build();
    if missing.is_empty() {
        group.set_description(Some("Every model in your logs has a price."));
    } else {
        group.set_description(Some(
            "These models are counted at $0. Type a catalog model to price each one like it, for example gemini-3-pro-preview, then press Enter.",
        ));
    }
    for (model, agent, requests, tokens) in missing {
        let row = adw::EntryRow::builder()
            .title(format!("{model}  ·  {}  ·  {} requests, {} tokens", agent.label(), format::count(requests), format::tokens(tokens)))
            .show_apply_button(true)
            .build();
        let c = Rc::downgrade(ctx);
        row.connect_apply(move |row| {
            let Some(c) = c.upgrade() else { return };
            let target = row.text().trim().to_string();
            if target.is_empty() {
                return;
            }
            if c.catalog.resolve(&target).price.is_none() {
                let hints: Vec<String> = c.catalog.search(&target, 3).into_iter().map(|p| p.id).collect();
                let hint = if hints.is_empty() { String::new() } else { format!(" Try {}.", hints.join(", ")) };
                c.toast(&format!("No price found for “{target}”.{hint}"));
                row.add_css_class("error");
                return;
            }
            match c.catalog.add_alias(&model, &target) {
                Ok(()) => {
                    row.remove_css_class("error");
                    row.set_editable(false);
                    row.set_show_apply_button(false);
                    row.add_suffix(&gtk::Image::from_icon_name("object-select-symbolic"));
                    c.send(ToWorker::Rebuild);
                    c.toast(&format!("{model} is now priced as {target}"));
                }
                Err(e) => c.toast(&format!("Could not save: {e}")),
            }
        });
        group.add(&row);
    }
    page.add(&group);
    dialog.present(Some(parent));
}
