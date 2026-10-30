// Main window layout and window actions.
//
// header: [new tab] [quick connect] [menu]
// body:   activity bar | side panel | (welcome | tabs) over bottom dock
// footer: status bar

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use vte::prelude::*;

use crate::dock;
use crate::editor;
use crate::palette;
use crate::pane;
use crate::recent::{self, Recent};
use crate::serial::SerialCfg;
use crate::sessions::{self, Quick};
use crate::sidebar::{OpenFn, Sidebar};
use crate::state::State;
use crate::store::{self, Entry, Folder};
use crate::status::Status;
use crate::term::{self, Launch};
use crate::welcome;

/// Keyboard shortcuts for window actions.
pub const ACCELS: &[(&str, &[&str])] = &[
    ("win.new-tab", &["<Ctrl><Shift>t"]),
    ("win.close-pane", &["<Ctrl><Shift>w"]),
    ("win.split-right", &["<Ctrl><Shift>e"]),
    ("win.split-down", &["<Ctrl><Shift>o"]),
    ("win.copy", &["<Ctrl><Shift>c"]),
    ("win.paste", &["<Ctrl><Shift>v"]),
    ("win.zoom-in", &["<Ctrl>plus", "<Ctrl>equal"]),
    ("win.zoom-out", &["<Ctrl>minus"]),
    ("win.zoom-reset", &["<Ctrl>0"]),
    ("win.toggle-sidebar", &["<Ctrl><Shift>b"]),
    ("win.toggle-dock", &["<Ctrl><Shift>j"]),
    ("win.quick-connect", &["<Ctrl><Shift>l"]),
    ("win.find-session", &["<Ctrl><Shift>k"]),
];

/// Widgets the actions need.
#[derive(Clone)]
struct Ui {
    win: adw::ApplicationWindow,
    view: adw::TabView,
    sidebar: Sidebar,
    dock: gtk::Box,
    status: Status,
    quick: gtk::Entry,
    toasts: adw::ToastOverlay,
    state: Rc<State>,
    /// Opens a session from a list, with error toast and recent tracking.
    pick: OpenFn,
}

impl Ui {
    fn toast(&self, msg: &str) {
        self.toasts.add_toast(adw::Toast::new(msg));
    }

    /// Focused terminal, else the first one in the selected tab.
    fn active(&self) -> Option<vte::Terminal> {
        let focused = gtk::prelude::GtkWindowExt::focus(&self.win)
            .and_then(|f| f.ancestor(vte::Terminal::static_type()))
            .and_then(|w| w.downcast::<vte::Terminal>().ok());
        focused.or_else(|| self.view.selected_page().and_then(|p| pane::find(&p.child())))
    }

    /// Opens a tab; `remember` adds it to the recent list.
    fn open(&self, launch: &Launch, remember: bool) {
        open_tab(&self.view, &self.state, launch);
        if remember {
            self.state.remember(launch);
        }
    }

    fn refresh_status(&self) {
        let launch = self.active().and_then(|t| self.state.launch(&t));
        let theme = self.state.theme().map(|t| t.name.clone()).unwrap_or_default();
        self.status.show(launch.as_ref(), &theme);
    }

    /// Launch of the first pane in each tab; shells keep their current dir.
    fn tabs(&self) -> Vec<Launch> {
        (0..self.view.n_pages())
            .filter_map(|i| {
                let t = pane::find(&self.view.nth_page(i).child())?;
                let mut l = self.state.launch(&t)?;
                if l.kind == term::Kind::Shell {
                    l.dir = term::cwd(&t).or(l.dir);
                }
                Some(l)
            })
            .collect()
    }

    fn export(&self) {
        let dlg = gtk::FileDialog::new();
        dlg.set_initial_name(Some("sessions.toml"));
        let u = self.clone();
        dlg.save(Some(&self.win), None::<&gio::Cancellable>, move |res| {
            let Some(path) = res.ok().and_then(|f| f.path()) else { return };
            match store::export(&u.state.store.borrow().root, &path) {
                Ok(()) => u.toast(&format!("Exported to {}", path.display())),
                Err(e) => u.toast(&format!("Export failed: {e}")),
            }
        });
    }

    /// Imports a TOML or JSON export into a new top-level folder.
    fn import(&self) {
        let u = self.clone();
        gtk::FileDialog::new().open(Some(&self.win), None::<&gio::Cancellable>, move |res| {
            let Some(path) = res.ok().and_then(|f| f.path()) else { return };
            let entries = match store::import(&path) {
                Ok(e) => e,
                Err(e) => return u.toast(&format!("Import failed: {e}")),
            };
            let n: usize = entries.iter().map(|e| 1 + e.count()).sum();
            let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let folder = Entry::Folder(Folder { name: format!("Imported {stem}"), children: entries, ..Default::default() });
            match u.state.edit_sessions(|s| s.insert(&[], None, folder)) {
                Ok(()) => u.toast(&format!("Imported {n} items")),
                Err(e) => u.toast(&e),
            }
        });
    }

    fn connect_quick(&self) {
        let text = self.quick.text();
        match sessions::quick(&text) {
            Some(Quick::Ssh { args, target }) => {
                self.open(&Launch::ssh(args, &target), true);
                self.quick.set_text("");
            }
            Some(Quick::Serial { device, baud }) => {
                let mut cfg = SerialCfg { device, ..Default::default() };
                if let Some(b) = baud {
                    cfg.baud = b;
                }
                self.open(&Launch::serial(cfg), true);
                self.quick.set_text("");
            }
            None => self.toast("Use user@host[:port], ssh <args> or /dev/ttyX[@baud]"),
        }
    }
}

fn open_tab(view: &adw::TabView, state: &Rc<State>, launch: &Launch) {
    let (t, leaf) = pane::open(state, launch);
    let root = pane::root();
    root.append(&leaf);
    let page = view.append(&root);
    page.set_title(&launch.title);
    page.set_icon(Some(&gio::ThemedIcon::new(launch.kind.icon())));
    view.set_selected_page(&page);
    t.grab_focus();
}

/// Builds and shows a window. Secondary windows (tab drag-out) close when empty.
pub fn build(app: &adw::Application, state: &Rc<State>, primary: bool) -> adw::TabView {
    let view = adw::TabView::new();
    let bar = adw::TabBar::new();
    bar.set_view(Some(&view));
    bar.set_autohide(false);

    let tabs = gtk::Box::new(gtk::Orientation::Vertical, 0);
    tabs.append(&bar);
    tabs.append(&view);
    view.set_vexpand(true);

    let main = gtk::Stack::new();
    main.set_vexpand(true);
    main.add_named(&welcome::build(), Some("welcome"));
    main.add_named(&tabs, Some("tabs"));
    main.set_visible_child_name("welcome");

    let dock = dock::build();
    let vsplit = gtk::Paned::new(gtk::Orientation::Vertical);
    vsplit.set_start_child(Some(&main));
    vsplit.set_end_child(Some(&dock));
    vsplit.set_resize_end_child(false);
    vsplit.set_shrink_end_child(false);
    vsplit.set_hexpand(true);

    let quick = gtk::Entry::new();
    quick.add_css_class("quick");
    quick.set_placeholder_text(Some("Quick connect: user@host:port, ssh <args>, /dev/ttyUSB0"));
    quick.set_primary_icon_name(Some("network-server-symbolic"));

    let header = adw::HeaderBar::new();
    let logo = gtk::Image::from_icon_name(crate::APP_ID);
    logo.set_pixel_size(24);
    logo.add_css_class("logo");
    header.pack_start(&logo);
    let add = gtk::Button::from_icon_name("tab-new-symbolic");
    add.set_action_name(Some("win.new-tab"));
    add.set_tooltip_text(Some("New Tab (Ctrl+Shift+T)"));
    header.pack_start(&add);
    header.set_title_widget(Some(&quick));
    let menu = gtk::MenuButton::new();
    menu.set_icon_name("open-menu-symbolic");
    menu.set_menu_model(Some(&menu_model(state)));
    header.pack_end(&menu);

    let status = Status::new();
    let toasts = adw::ToastOverlay::new();
    let body = adw::ToolbarView::new();
    body.add_top_bar(&header);
    let strip = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    strip.add_css_class("strip");
    body.add_top_bar(&strip);
    body.set_content(Some(&toasts));

    let win = adw::ApplicationWindow::builder()
        .application(app)
        .title("termvisor")
        .default_width(1200)
        .default_height(760)
        .content(&body)
        .build();

    let weak = view.downgrade();
    let st = state.clone();
    let tw = toasts.downgrade();
    let open_cb: OpenFn = Rc::new(move |launch: Result<Launch, String>| match launch {
        Ok(l) => {
            if let Some(v) = weak.upgrade() {
                open_tab(&v, &st, &l);
                st.remember(&l);
            }
        }
        Err(e) => {
            if let Some(t) = tw.upgrade() {
                t.add_toast(adw::Toast::new(&e));
            }
        }
    });
    let sidebar = Sidebar::new(state, open_cb.clone());

    let hsplit = gtk::Paned::new(gtk::Orientation::Horizontal);
    hsplit.set_start_child(Some(&sidebar.panel));
    hsplit.set_end_child(Some(&vsplit));
    hsplit.set_resize_start_child(false);
    hsplit.set_shrink_start_child(false);

    let center = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    center.set_vexpand(true);
    center.append(&sidebar.bar);
    center.append(&hsplit);

    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    outer.append(&center);
    outer.append(&status.bar);
    toasts.set_child(Some(&outer));

    let ui = Ui {
        win: win.clone(),
        view: view.clone(),
        sidebar,
        dock,
        status,
        quick: quick.clone(),
        toasts,
        state: state.clone(),
        pick: open_cb,
    };
    add_actions(&ui);

    let u = ui.clone();
    quick.connect_activate(move |_| u.connect_quick());

    let weak = win.downgrade();
    view.connect_n_pages_notify(move |v| {
        if v.n_pages() > 0 {
            main.set_visible_child_name("tabs");
            return;
        }
        if primary {
            main.set_visible_child_name("welcome");
        } else if let Some(w) = weak.upgrade() {
            w.close();
        }
    });

    let app_ref = app.clone();
    let st = state.clone();
    view.connect_create_window(move |_| Some(build(&app_ref, &st, false)));

    let u = ui.clone();
    view.connect_selected_page_notify(move |v| {
        if let Some(t) = v.selected_page().and_then(|p| pane::find(&p.child())) {
            t.grab_focus();
        }
        u.refresh_status();
    });
    let u = ui.clone();
    win.connect_focus_widget_notify(move |_| u.refresh_status());

    if primary {
        let u = ui.clone();
        win.connect_close_request(move |_| {
            let list: Vec<Recent> = if u.state.cfg.borrow().restore_tabs {
                u.tabs().iter().map(Recent::of).collect()
            } else {
                Vec::new()
            };
            if let Err(e) = recent::save(recent::TABS, &list) {
                crate::warn(&format!("save tabs: {e}"));
            }
            glib::Propagation::Proceed
        });
        if state.cfg.borrow().restore_tabs {
            for r in recent::load(recent::TABS) {
                ui.open(&r.launch(), false);
            }
        }
    }

    ui.refresh_status();
    win.present();
    view
}

fn menu_model(state: &Rc<State>) -> gio::Menu {
    let panes = gio::Menu::new();
    panes.append(Some("Split Right"), Some("win.split-right"));
    panes.append(Some("Split Down"), Some("win.split-down"));
    panes.append(Some("Close Pane"), Some("win.close-pane"));

    let view = gio::Menu::new();
    view.append(Some("Toggle Sidebar"), Some("win.toggle-sidebar"));
    view.append(Some("Toggle Bottom Panel"), Some("win.toggle-dock"));

    let sess = gio::Menu::new();
    sess.append(Some("Find Session…"), Some("win.find-session"));
    sess.append(Some("Import Sessions…"), Some("win.import"));
    sess.append(Some("Export Sessions…"), Some("win.export"));
    sess.append(Some("Restore Tabs on Start"), Some("win.restore-tabs"));

    let themes = gio::Menu::new();
    for t in &state.themes {
        let item = gio::MenuItem::new(Some(&t.name), None);
        item.set_action_and_target_value(Some("win.theme"), Some(&t.name.to_variant()));
        themes.append_item(&item);
    }

    let menu = gio::Menu::new();
    menu.append_section(None, &sess);
    menu.append_section(None, &panes);
    menu.append_section(None, &view);
    menu.append_submenu(Some("Theme"), &themes);
    menu
}

fn action<F>(ui: &Ui, name: &str, param: Option<&glib::VariantTy>, f: F)
where
    F: Fn(&Ui, Option<&glib::Variant>) + 'static,
{
    let a = gio::SimpleAction::new(name, param);
    let u = ui.clone();
    a.connect_activate(move |_, p| f(&u, p));
    ui.win.add_action(&a);
}

fn on_term<F>(ui: &Ui, name: &str, f: F)
where
    F: Fn(&Ui, &vte::Terminal) + 'static,
{
    action(ui, name, None, move |u, _| {
        if let Some(t) = u.active() {
            f(u, &t);
        }
    });
}

fn add_actions(ui: &Ui) {
    action(ui, "new-tab", None, |u, _| {
        let dir = u.active().and_then(|t| term::cwd(&t));
        u.open(&Launch::shell(dir), false);
    });
    action(ui, "find-session", None, |u, _| {
        let items = sessions::flat(&sessions::base(&u.state.store.borrow().root));
        palette::open(&u.win, items, u.pick.clone());
    });
    action(ui, "new-serial", None, |u, _| {
        let cfg = editor::default_serial();
        let name = cfg.device.rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or("Serial").to_string();
        let init = Entry::Serial(store::Serial { name, cfg, ..Default::default() });
        editor::open(&u.win, &u.state, editor::Mode::New(Vec::new()), init);
    });
    action(ui, "import", None, |u, _| u.import());
    action(ui, "export", None, |u, _| u.export());
    let restore = gio::SimpleAction::new_stateful("restore-tabs", None, &ui.state.cfg.borrow().restore_tabs.to_variant());
    let u = ui.clone();
    restore.connect_activate(move |a, _| {
        let on = !a.state().and_then(|v| v.get::<bool>()).unwrap_or(false);
        u.state.set_restore(on);
        a.set_state(&on.to_variant());
    });
    ui.win.add_action(&restore);
    action(ui, "quick-connect", None, |u, _| {
        u.quick.grab_focus();
    });
    action(ui, "soon", Some(glib::VariantTy::STRING), |u, p| {
        if let Some(msg) = p.and_then(|v| v.get::<String>()) {
            u.toast(&msg);
        }
    });
    action(ui, "toggle-sidebar", None, |u, _| u.sidebar.toggle());
    action(ui, "toggle-dock", None, |u, _| u.dock.set_visible(!u.dock.is_visible()));

    on_term(ui, "split-right", |u, t| {
        pane::split(&u.state, t, gtk::Orientation::Horizontal);
    });
    on_term(ui, "split-down", |u, t| {
        pane::split(&u.state, t, gtk::Orientation::Vertical);
    });
    on_term(ui, "close-pane", |_, t| pane::close(t));
    on_term(ui, "copy", |_, t| t.copy_clipboard_format(vte::Format::Text));
    on_term(ui, "paste", |_, t| t.paste_clipboard());
    on_term(ui, "zoom-in", |_, t| t.set_font_scale((t.font_scale() * 1.1).min(4.0)));
    on_term(ui, "zoom-out", |_, t| t.set_font_scale((t.font_scale() / 1.1).max(0.25)));
    on_term(ui, "zoom-reset", |_, t| t.set_font_scale(1.0));

    let current = ui.state.theme().map(|t| t.name.clone()).unwrap_or_default();
    let theme = gio::SimpleAction::new_stateful("theme", Some(glib::VariantTy::STRING), &current.to_variant());
    let u = ui.clone();
    theme.connect_activate(move |a, p| {
        let Some(name) = p.and_then(|v| v.get::<String>()) else {
            return;
        };
        u.state.set_theme(&name);
        a.set_state(&name.to_variant());
        u.refresh_status();
    });
    ui.win.add_action(&theme);
}
