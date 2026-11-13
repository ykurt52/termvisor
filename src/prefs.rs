// Preferences window. Every change is applied and saved at once.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib, glib::translate::IntoGlib};

use crate::keybindings::{self, Overrides, ACTIONS};
use crate::output::LogMode;
use crate::settings::ThemeRule;
use crate::state::State;

const RENDERERS: [&str; 5] = ["auto", "cairo", "gl", "ngl", "vulkan"];

fn combo(title: &str, items: &[String], at: usize) -> adw::ComboRow {
    let r = adw::ComboRow::new();
    r.set_title(title);
    let refs: Vec<&str> = items.iter().map(String::as_str).collect();
    r.set_model(Some(&gtk::StringList::new(&refs)));
    r.set_selected(at.min(items.len().saturating_sub(1)) as u32);
    r
}

fn switch(title: &str, subtitle: &str, on: bool) -> adw::SwitchRow {
    let r = adw::SwitchRow::new();
    r.set_title(title);
    if !subtitle.is_empty() {
        r.set_subtitle(subtitle);
    }
    r.set_active(on);
    r
}

/// Fixed-width font families, sorted; `keep` is added when missing.
fn mono_families(w: &gtk::Widget, keep: &str) -> Vec<String> {
    let mut list: Vec<String> = w
        .pango_context()
        .list_families()
        .iter()
        .filter(|f| f.is_monospace())
        .map(|f| f.name().to_string())
        // Fonts that report fixed width but are not for text.
        .filter(|n| !n.contains("Emoji") && !n.contains("MathJax") && !n.contains("SignWriting"))
        .collect();
    if !list.iter().any(|n| n == keep) {
        list.push(keep.to_string());
    }
    list.sort_by_key(|n| n.to_lowercase());
    list.dedup();
    list
}

fn names(state: &State) -> Vec<String> {
    state.themes().into_iter().map(|t| t.name).collect()
}

fn index(list: &[String], name: &str) -> usize {
    list.iter().position(|n| n == name).unwrap_or(0)
}

fn toast(dlg: &adw::PreferencesDialog, msg: &str) {
    dlg.add_toast(adw::Toast::new(msg));
}

fn appearance(state: &Rc<State>, dlg: &adw::PreferencesDialog) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Appearance");
    page.set_icon_name(Some("applications-graphics-symbolic"));
    let cfg = state.cfg.borrow().clone();
    let all = names(state);

    let themes = adw::PreferencesGroup::new();
    themes.set_title("Theme");
    let theme = combo("Theme", &all, index(&all, &cfg.theme));
    let follow = switch("Follow System Style", "Use the themes below when the system is light or dark", cfg.follow_system);
    let light = combo("Light Style Theme", &all, index(&all, &cfg.theme_light));
    let dark = combo("Dark Style Theme", &all, index(&all, &cfg.theme_dark));
    let edit = adw::ActionRow::new();
    edit.set_title("Edit Theme…");
    edit.set_subtitle("Change colors with a live preview");
    edit.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    edit.set_activatable(true);
    edit.set_action_name(Some("win.edit-theme"));
    for w in [theme.upcast_ref::<gtk::Widget>(), follow.upcast_ref(), light.upcast_ref(), dark.upcast_ref(), edit.upcast_ref()] {
        themes.add(w);
    }
    let sync = {
        let (theme, light, dark) = (theme.clone(), light.clone(), dark.clone());
        move |on: bool| {
            theme.set_sensitive(!on);
            light.set_sensitive(on);
            dark.set_sensitive(on);
        }
    };
    sync(cfg.follow_system);
    let (st, l) = (state.clone(), all.clone());
    theme.connect_selected_notify(move |c| {
        if let Some(n) = l.get(c.selected() as usize) {
            st.set_theme(n);
        }
    });
    let st = state.clone();
    follow.connect_active_notify(move |r| {
        let on = r.is_active();
        st.edit_cfg(|c| c.follow_system = on);
        sync(on);
        st.retheme();
    });
    for (row, is_dark) in [(light.clone(), false), (dark.clone(), true)] {
        let (st, l) = (state.clone(), all.clone());
        row.connect_selected_notify(move |c| {
            let Some(n) = l.get(c.selected() as usize).cloned() else { return };
            st.edit_cfg(|cfg| if is_dark { cfg.theme_dark = n } else { cfg.theme_light = n });
            st.retheme();
        });
    }
    page.add(&themes);

    let term = adw::PreferencesGroup::new();
    term.set_title("Terminal");
    // Fixed-width families straight from Pango: instant, unlike the
    // general font dialog, which prepares a preview of every face first.
    let desc = gtk::pango::FontDescription::from_string(&cfg.font);
    let current = desc.family().map(|f| f.to_string()).unwrap_or_else(|| "Monospace".into());
    let families = mono_families(dlg.upcast_ref(), &current);
    let family = combo("Font", &families, index(&families, &current));
    family.set_enable_search(true);
    let size = adw::SpinRow::with_range(6.0, 48.0, 1.0);
    size.set_title("Font Size");
    let points = f64::from(desc.size()) / f64::from(gtk::pango::SCALE);
    size.set_value(if points > 0.0 { points } else { 11.0 });
    let apply = {
        let (st, family, size, families) = (state.clone(), family.clone(), size.clone(), families.clone());
        move || {
            let Some(f) = families.get(family.selected() as usize) else { return };
            let font = format!("{f} {}", size.value() as u32);
            st.edit_cfg(|c| c.font = font);
            st.apply_terminal_prefs();
        }
    };
    let a = apply.clone();
    family.connect_selected_notify(move |_| a());
    size.connect_value_notify(move |_| apply());
    let lines = adw::SpinRow::with_range(1000.0, 1_000_000.0, 1000.0);
    lines.set_title("Scrollback Lines");
    lines.set_value(cfg.scrollback as f64);
    let st = state.clone();
    lines.connect_value_notify(move |r| {
        st.edit_cfg(|c| c.scrollback = r.value() as i64);
        st.apply_terminal_prefs();
    });
    let rend: Vec<String> = RENDERERS.iter().map(|s| s.to_string()).collect();
    let renderer = combo("Renderer", &rend, index(&rend, &cfg.renderer));
    renderer.set_subtitle("auto picks the CPU renderer on software OpenGL; applies on restart");
    let st = state.clone();
    renderer.connect_selected_notify(move |c| {
        let n = RENDERERS[(c.selected() as usize).min(RENDERERS.len() - 1)];
        st.edit_cfg(|cfg| cfg.renderer = n.into());
    });
    term.add(&family);
    term.add(&size);
    term.add(&lines);
    term.add(&renderer);
    page.add(&term);

    page.add(&rules(state, dlg));
    page
}

/// Shows the session theme rules.
fn fill_rules(list: &gtk::ListBox, state: &Rc<State>) {
    list.remove_all();
    let rules = state.cfg.borrow().theme_rules.clone();
    list.set_visible(!rules.is_empty());
    for (i, r) in rules.iter().enumerate() {
        let row = adw::ActionRow::new();
        row.set_title(&glib::markup_escape_text(&r.sessions));
        row.set_subtitle(&glib::markup_escape_text(&r.theme));
        let del = gtk::Button::from_icon_name("user-trash-symbolic");
        del.add_css_class("flat");
        del.set_valign(gtk::Align::Center);
        row.add_suffix(&del);
        let (st, l) = (state.clone(), list.downgrade());
        del.connect_clicked(move |_| {
            st.edit_cfg(|c| {
                if i < c.theme_rules.len() {
                    c.theme_rules.remove(i);
                }
            });
            st.retheme();
            if let Some(l) = l.upgrade() {
                fill_rules(&l, &st);
            }
        });
        list.append(&row);
    }
}

/// Session theme rules: glob patterns -> theme.
fn rules(state: &Rc<State>, dlg: &adw::PreferencesDialog) -> adw::PreferencesGroup {
    let g = adw::PreferencesGroup::new();
    g.set_title("Session Themes");
    g.set_description(Some("Panes whose title or target match get their own theme, e.g. root@prod* in red."));
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    g.add(&list);
    let add = gtk::Button::with_label("Add Rule…");
    add.set_halign(gtk::Align::Start);
    add.set_margin_top(6);
    g.add(&add);
    fill_rules(&list, state);
    let (st, d, l) = (state.clone(), dlg.clone(), list.downgrade());
    add.connect_clicked(move |_| {
        let a = adw::AlertDialog::new(Some("Session Theme"), None);
        let pat = gtk::Entry::new();
        pat.set_placeholder_text(Some("Patterns, e.g. root@prod*, /dev/ttyUSB*"));
        let all = names(&st);
        let refs: Vec<&str> = all.iter().map(String::as_str).collect();
        let pick = gtk::DropDown::from_strings(&refs);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 6);
        body.append(&pat);
        body.append(&pick);
        a.set_extra_child(Some(&body));
        a.add_responses(&[("cancel", "Cancel"), ("add", "Add")]);
        a.set_response_appearance("add", adw::ResponseAppearance::Suggested);
        a.set_default_response(Some("add"));
        a.set_close_response("cancel");
        let (st, l) = (st.clone(), l.clone());
        a.connect_response(None, move |_, r| {
            let sessions = pat.text().trim().to_string();
            if r != "add" || sessions.is_empty() {
                return;
            }
            let theme = all.get(pick.selected() as usize).cloned().unwrap_or_default();
            st.edit_cfg(|c| c.theme_rules.push(ThemeRule { sessions, theme }));
            st.retheme();
            if let Some(l) = l.upgrade() {
                fill_rules(&l, &st);
            }
        });
        a.present(Some(&d));
    });
    g
}

/// "Check Now": the result shows in the row. With a newer release the
/// button becomes Update…, which closes Preferences and offers it in the
/// window.
fn check_row(dlg: &adw::PreferencesDialog, host: &gtk::Widget) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    row.set_title("Updates");
    row.set_subtitle(&format!("termvisor {}", env!("CARGO_PKG_VERSION")));
    let button = gtk::Button::with_label("Check Now");
    button.set_valign(gtk::Align::Center);
    row.add_suffix(&button);
    let found = Rc::new(Cell::new(false));
    let (r, d, h) = (row.downgrade(), dlg.downgrade(), host.downgrade());
    button.connect_clicked(move |b| {
        if found.get() {
            if let (Some(d), Some(h)) = (d.upgrade(), h.upgrade()) {
                d.close();
                let _ = WidgetExt::activate_action(&h, "win.check-updates", None);
            }
            return;
        }
        let Some(row) = r.upgrade() else { return };
        row.set_subtitle("Checking…");
        b.set_sensitive(false);
        let (r, b, found) = (row.downgrade(), b.downgrade(), found.clone());
        crate::update::latest(move |res| {
            let (Some(row), Some(b)) = (r.upgrade(), b.upgrade()) else { return };
            b.set_sensitive(true);
            match res {
                Ok(rel) if crate::update::is_newer(&rel) => {
                    row.set_subtitle(&format!("termvisor {} is available", rel.version));
                    b.set_label("Update…");
                    b.add_css_class("suggested-action");
                    found.set(true);
                }
                Ok(_) => row.set_subtitle(&format!("termvisor {} is up to date", env!("CARGO_PKG_VERSION"))),
                Err(e) => row.set_subtitle(&format!("Check failed: {e}")),
            }
        });
    });
    row
}

fn panes(state: &Rc<State>, dlg: &adw::PreferencesDialog, host: &gtk::Widget) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Panes");
    page.set_icon_name(Some("view-dual-symbolic"));
    let cfg = state.cfg.borrow().clone();

    let new = adw::PreferencesGroup::new();
    new.set_title("New Panes");
    new.set_description(Some("The dock's Log page changes a single pane."));
    let stamps = switch("Timestamps", "Time before each line", cfg.timestamps);
    let marks = switch("Highlight", "Color matches of the highlight rules", cfg.highlight);
    let modes: Vec<String> = LogMode::ALL.iter().map(|m| m.label().to_string()).collect();
    let log = combo("Log to File", &modes, LogMode::ALL.iter().position(|m| *m == cfg.log).unwrap_or(0));
    let st = state.clone();
    stamps.connect_active_notify(move |r| {
        let on = r.is_active();
        st.edit_cfg(|c| c.timestamps = on);
    });
    let st = state.clone();
    marks.connect_active_notify(move |r| {
        let on = r.is_active();
        st.edit_cfg(|c| c.highlight = on);
    });
    let st = state.clone();
    log.connect_selected_notify(move |c| {
        let m = LogMode::ALL.get(c.selected() as usize).copied().unwrap_or_default();
        st.edit_cfg(|cfg| cfg.log = m);
    });
    let dir = adw::ActionRow::new();
    dir.set_title("Log Folder");
    dir.set_subtitle(&if cfg.log_dir.is_empty() { crate::output::log_dir().display().to_string() } else { cfg.log_dir.clone() });
    let choose = gtk::Button::from_icon_name("folder-open-symbolic");
    choose.add_css_class("flat");
    choose.set_valign(gtk::Align::Center);
    choose.set_tooltip_text(Some("Choose"));
    let reset = gtk::Button::from_icon_name("edit-undo-symbolic");
    reset.add_css_class("flat");
    reset.set_valign(gtk::Align::Center);
    reset.set_tooltip_text(Some("Default folder"));
    dir.add_suffix(&choose);
    dir.add_suffix(&reset);
    let (st, row) = (state.clone(), dir.clone());
    choose.connect_clicked(move |b| {
        let (st, row) = (st.clone(), row.clone());
        gtk::FileDialog::new().select_folder(b.root().and_downcast::<gtk::Window>().as_ref(), None::<&gio::Cancellable>, move |res| {
            if let Some(p) = res.ok().and_then(|f| f.path()) {
                let s = p.to_string_lossy().into_owned();
                st.edit_cfg(|c| c.log_dir = s.clone());
                row.set_subtitle(&s);
            }
        });
    });
    let (st, row) = (state.clone(), dir.clone());
    reset.connect_clicked(move |_| {
        st.edit_cfg(|c| c.log_dir.clear());
        row.set_subtitle(&crate::output::log_dir().display().to_string());
    });
    for w in [stamps.upcast_ref::<gtk::Widget>(), marks.upcast_ref(), log.upcast_ref(), dir.upcast_ref()] {
        new.add(w);
    }
    page.add(&new);

    let win = adw::PreferencesGroup::new();
    win.set_title("Window");
    let restore = switch("Restore Tabs on Start", "", cfg.restore_tabs);
    let right = switch("Panel on Right", "Commands, transfers and log beside the tabs", cfg.dock_right);
    let fkeys = switch("F1–F12 Send Command Buttons", "Off by default: full-screen programs use these keys", cfg.command_keys);
    let updates = switch("Check for Updates", "On every start, asks GitHub for the latest release", cfg.update_check);
    let st = state.clone();
    restore.connect_active_notify(move |r| st.set_restore(r.is_active()));
    // Through the window action, so the window moves its panel.
    let (st, h) = (state.clone(), host.downgrade());
    right.connect_active_notify(move |r| {
        let differs = st.cfg.borrow().dock_right != r.is_active();
        if let (true, Some(h)) = (differs, h.upgrade()) {
            let _ = WidgetExt::activate_action(&h, "win.dock-right", None);
        }
    });
    let st = state.clone();
    fkeys.connect_active_notify(move |r| {
        let on = r.is_active();
        st.edit_cfg(|c| c.command_keys = on);
        st.commands_changed();
    });
    let st = state.clone();
    updates.connect_active_notify(move |r| {
        let on = r.is_active();
        st.edit_cfg(|c| c.update_check = on);
    });
    for w in [restore.upcast_ref::<gtk::Widget>(), right.upcast_ref(), fkeys.upcast_ref(), updates.upcast_ref()] {
        win.add(w);
    }
    win.add(&check_row(dlg, host));
    page.add(&win);

    let android = adw::PreferencesGroup::new();
    android.set_title("Android");
    let adb = adw::EntryRow::new();
    adb.set_title("adb Program (name or full path)");
    adb.set_text(&cfg.adb);
    adb.set_show_apply_button(true);
    let (st, d) = (state.clone(), dlg.clone());
    adb.connect_apply(move |e| {
        let v = e.text().trim().to_string();
        let v = if v.is_empty() { "adb".to_string() } else { v };
        st.edit_cfg(|c| c.adb = v.clone());
        toast(&d, &if crate::adb::found(&v) { format!("Using {v}") } else { format!("{v} not found") });
    });
    android.add(&adb);
    page.add(&android);
    page
}

fn keyboard(dlg: &adw::PreferencesDialog) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Keyboard");
    page.set_icon_name(Some("input-keyboard-symbolic"));
    let g = adw::PreferencesGroup::new();
    g.set_title("Shortcuts");
    g.set_description(Some("Set… then press the keys; Escape cancels, Backspace removes the shortcut."));
    let reset_all = gtk::Button::with_label("Reset All");
    reset_all.add_css_class("flat");
    g.set_header_suffix(Some(&reset_all));

    let map: Rc<RefCell<Overrides>> = Rc::new(RefCell::new(keybindings::load()));
    // Action being recorded, with its label and button.
    let recording: Rc<RefCell<Option<(String, gtk::ShortcutLabel, gtk::ToggleButton)>>> = Rc::default();
    let mut labels = Vec::new();
    for (action, label, _) in ACTIONS {
        let row = adw::ActionRow::new();
        row.set_title(label);
        let shown = gtk::ShortcutLabel::new(&keybindings::keys(&map.borrow(), action).join(" "));
        shown.set_disabled_text("None");
        shown.set_valign(gtk::Align::Center);
        let set = gtk::ToggleButton::with_label("Set…");
        set.set_valign(gtk::Align::Center);
        let undo = gtk::Button::from_icon_name("edit-undo-symbolic");
        undo.add_css_class("flat");
        undo.set_valign(gtk::Align::Center);
        undo.set_tooltip_text(Some("Default"));
        row.add_suffix(&shown);
        row.add_suffix(&set);
        row.add_suffix(&undo);
        g.add(&row);
        labels.push((action.to_string(), shown.clone()));

        let (rec, a, l) = (recording.clone(), action.to_string(), shown.clone());
        set.connect_toggled(move |b| {
            if b.is_active() {
                b.set_label("Press keys…");
                // One at a time.
                if let Some((_, _, other)) = rec.replace(Some((a.clone(), l.clone(), b.clone()))) {
                    other.set_active(false);
                }
            } else {
                b.set_label("Set…");
                let same = rec.borrow().as_ref().is_some_and(|(x, _, _)| *x == a);
                if same {
                    rec.replace(None);
                }
            }
        });
        let (m, a, l, d) = (map.clone(), action.to_string(), shown.clone(), dlg.clone());
        undo.connect_clicked(move |_| {
            m.borrow_mut().remove(&a);
            store(&d, &m.borrow());
            l.set_accelerator(&keybindings::keys(&m.borrow(), &a).join(" "));
        });
    }
    let (m, d) = (map.clone(), dlg.clone());
    reset_all.connect_clicked(move |_| {
        m.borrow_mut().clear();
        store(&d, &m.borrow());
        for (a, l) in &labels {
            l.set_accelerator(&keybindings::keys(&m.borrow(), a).join(" "));
        }
    });

    let catch = gtk::EventControllerKey::new();
    catch.set_propagation_phase(gtk::PropagationPhase::Capture);
    let (m, d) = (map.clone(), dlg.clone());
    catch.connect_key_pressed(move |_, keyval, _, state| {
        let Some((action, label, button)) = recording.borrow().clone() else { return glib::Propagation::Proceed };
        let mods = state & gtk::accelerator_get_default_mod_mask();
        let done = |b: &gtk::ToggleButton| b.set_active(false);
        if keyval == gdk::Key::Escape && mods.is_empty() {
            done(&button);
            return glib::Propagation::Stop;
        }
        if keyval == gdk::Key::BackSpace && mods.is_empty() {
            m.borrow_mut().insert(action.clone(), Vec::new());
        } else {
            if !gtk::accelerator_valid(keyval, mods) {
                return glib::Propagation::Stop;
            }
            let v = keyval.into_glib();
            let fkey = (gdk::Key::F1.into_glib()..=gdk::Key::F35.into_glib()).contains(&v);
            let held = gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK | gdk::ModifierType::SUPER_MASK;
            if !fkey && !mods.intersects(held) {
                toast(&d, "Use Ctrl, Alt or Super with this key, or a function key");
                return glib::Propagation::Stop;
            }
            m.borrow_mut().insert(action.clone(), vec![gtk::accelerator_name(keyval, mods).to_string()]);
        }
        store(&d, &m.borrow());
        label.set_accelerator(&keybindings::keys(&m.borrow(), &action).join(" "));
        done(&button);
        glib::Propagation::Stop
    });
    dlg.add_controller(catch);
    page.add(&g);
    page
}

/// Saves and applies shortcuts; warns about keys used twice.
fn store(dlg: &adw::PreferencesDialog, map: &Overrides) {
    if let Err(e) = keybindings::save(map) {
        toast(dlg, &format!("Save shortcuts: {e}"));
    }
    if let Some(app) = gio::Application::default().and_downcast::<gtk::Application>() {
        keybindings::apply(&app, map);
    }
    for (key, actions) in keybindings::conflicts(map) {
        toast(dlg, &format!("{key} is used by {}", actions.join(" and ")));
    }
}

fn files(dlg: &adw::PreferencesDialog) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Files");
    page.set_icon_name(Some("folder-symbolic"));
    let g = adw::PreferencesGroup::new();
    g.set_title("Configuration");
    g.set_description(Some("All settings are plain files; edits to themes apply at once, others on restart."));
    for (title, path) in [
        ("settings.json", crate::settings::dir().join("settings.json")),
        ("keybindings.json", keybindings::path()),
        ("Themes folder", crate::state::theme_dir()),
        ("Configuration folder", crate::settings::dir()),
    ] {
        let row = adw::ActionRow::new();
        row.set_title(title);
        row.set_subtitle(&path.display().to_string());
        let open = gtk::Button::from_icon_name("document-open-symbolic");
        open.add_css_class("flat");
        open.set_valign(gtk::Align::Center);
        open.set_tooltip_text(Some("Open"));
        row.add_suffix(&open);
        let d = dlg.clone();
        open.connect_clicked(move |b| {
            if path.extension().is_some() && !path.exists() {
                let _ = std::fs::write(&path, "{}\n");
            }
            if path.extension().is_none() {
                let _ = std::fs::create_dir_all(&path);
            }
            let l = gtk::FileLauncher::new(Some(&gio::File::for_path(&path)));
            let d = d.clone();
            l.launch(b.root().and_downcast::<gtk::Window>().as_ref(), None::<&gio::Cancellable>, move |r| {
                if let Err(e) = r {
                    toast(&d, &e.to_string());
                }
            });
        });
        g.add(&row);
    }
    page.add(&g);
    page
}

pub fn open(parent: &impl IsA<gtk::Widget>, state: &Rc<State>) {
    let dlg = adw::PreferencesDialog::new();
    dlg.set_title("Preferences");
    dlg.set_search_enabled(true);
    dlg.add(&appearance(state, &dlg));
    dlg.add(&panes(state, &dlg, parent.upcast_ref()));
    dlg.add(&keyboard(&dlg));
    dlg.add(&files(&dlg));
    dlg.present(Some(parent));
}
