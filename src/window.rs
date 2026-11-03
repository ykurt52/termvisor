// Main window layout and window actions.
//
// header: [new tab] [quick connect] [menu]
// body:   activity bar | side panel | (welcome | tabs) over bottom dock
// footer: status bar

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::dock;
use crate::multi;
use crate::editor;
use crate::palette;
use crate::pane;
use crate::recover;
use crate::streampane;
use crate::recent::{self, Recent};
use crate::ftp::FtpCfg;
use crate::keyring;
use crate::link::Eol;
use crate::remote::Target;
use crate::net::NetCfg;
use crate::serial::SerialCfg;
use crate::sessions::{self, Quick};
use crate::sidebar::{OpenFn, Sidebar};
use crate::state::State;
use crate::store::{self, Entry, Folder};
use crate::status::Status;
use crate::term::{self, Launch, Stream};
use crate::welcome;

/// Widgets the actions need.
#[derive(Clone)]
struct Ui {
    win: adw::ApplicationWindow,
    view: adw::TabView,
    sidebar: Sidebar,
    dock: Rc<dock::Dock>,
    vsplit: gtk::Paned,
    /// Last terminal that had the focus.
    last: Rc<glib::WeakRef<crate::vt::View>>,
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

    fn active(&self) -> Option<crate::vt::View> {
        active(&self.win, &self.view, &self.last)
    }

    /// Opens a tab; `remember` adds it to the recent list.
    fn open(&self, launch: &Launch, remember: bool) {
        open_tab(&self.view, &self.state, launch);
        if remember {
            self.state.remember(launch);
        }
    }

    /// Puts the dock below the tabs or on their right.
    fn place_dock(&self, right: bool) {
        let o = if right { gtk::Orientation::Horizontal } else { gtk::Orientation::Vertical };
        self.vsplit.set_orientation(o);
        self.dock.set_right(right);
        // A negative position unsets it; the dock gets its requested size.
        self.vsplit.set_position(-1);
    }

    fn refresh_status(&self) {
        let launch = self.active().and_then(|t| self.state.launch(&t));
        let theme = self.state.theme().map(|t| t.name.clone()).unwrap_or_default();
        self.status.show(launch.as_ref(), &theme);
        self.sidebar.files.attach(launch.as_ref());
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

    /// Confirms, then releases serial ports, shared SSH connections and
    /// optionally other instances. The slow part runs off the GUI thread.
    fn release(&self) {
        let ports = streampane::open_count();
        let masters = recover::masters();
        let others = recover::instances();
        let mut lines = vec![
            format!("• close {ports} serial port(s) in this window; panes stay open"),
            format!("• close {masters} shared SSH connection(s); SSH panes can reconnect with Enter"),
            format!("• stop {} SSH tunnel(s)", self.state.tunnels.active()),
            "• remove stale lock files and sockets".to_string(),
        ];
        if others.is_empty() {
            lines.push("• no other termvisor instance is running".into());
        }
        let d = adw::AlertDialog::new(Some("Release All Connections?"), Some(&lines.join("\n")));
        let stop_others = gtk::CheckButton::new();
        if !others.is_empty() {
            let desc: Vec<String> = others
                .iter()
                .map(|i| {
                    if i.ports.is_empty() {
                        format!("PID {}", i.pid)
                    } else {
                        format!("PID {} ({})", i.pid, i.ports.join(", "))
                    }
                })
                .collect();
            stop_others.set_label(Some(&format!("Also stop other termvisor instances: {}", desc.join(", "))));
            stop_others.set_active(others.iter().any(|i| !i.ports.is_empty()));
            d.set_extra_child(Some(&stop_others));
        }
        d.add_responses(&[("cancel", "Cancel"), ("release", "Release")]);
        d.set_response_appearance("release", adw::ResponseAppearance::Destructive);
        d.set_default_response(Some("cancel"));
        d.set_close_response("cancel");
        let u = self.clone();
        d.connect_response(None, move |_, resp| {
            if resp != "release" {
                return;
            }
            let n = streampane::release_all();
            u.state.tunnels.stop_all();
            let kill = stop_others.is_active();
            let u = u.clone();
            glib::spawn_future_local(async move {
                let msg = match gio::spawn_blocking(move || recover::release(kill)).await {
                    Ok(r) => format!("{n} serial port(s) released; {}", r.summary()),
                    Err(_) => "Release failed".into(),
                };
                u.toast(&msg);
            });
        });
        d.present(Some(&self.win));
    }

    /// Opens an FTP server in the Files panel, asking for the password
    /// unless it is anonymous or stored in the keyring.
    fn open_ftp(&self, title: &str, cfg: FtpCfg, use_keyring: bool) {
        let label = format!("{title} · {}", cfg.id());
        let go = {
            let (u, label, cfg) = (self.clone(), label.clone(), cfg.clone());
            move |pass: String| {
                u.sidebar.show("files");
                u.sidebar.files.open_target(Target::Ftp(cfg.clone(), pass), &label);
            }
        };
        if cfg.anonymous() {
            return go(String::new());
        }
        let id = keyring::password_id(&cfg.id());
        if use_keyring {
            if let Some(p) = keyring::get(&id) {
                return go(p);
            }
        }
        let d = adw::AlertDialog::new(Some("FTP Password"), Some(&cfg.id()));
        let pass = gtk::PasswordEntry::new();
        pass.set_show_peek_icon(true);
        pass.set_activates_default(true);
        let remember = gtk::CheckButton::with_label("Remember in keyring");
        remember.set_active(true);
        remember.set_visible(use_keyring);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
        body.append(&pass);
        body.append(&remember);
        d.set_extra_child(Some(&body));
        d.add_responses(&[("cancel", "Cancel"), ("connect", "Connect")]);
        d.set_response_appearance("connect", adw::ResponseAppearance::Suggested);
        d.set_default_response(Some("connect"));
        d.set_close_response("cancel");
        let u = self.clone();
        d.connect_response(None, move |_, r| {
            if r != "connect" {
                return;
            }
            let p = pass.text().to_string();
            if remember.is_active() && use_keyring {
                if let Err(e) = keyring::set(&id, &p) {
                    u.toast(&e);
                }
            }
            go(p);
        });
        d.present(Some(&self.win));
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
                self.open(&Launch::stream(Stream::Serial(cfg)), true);
                self.quick.set_text("");
            }
            Some(Quick::Net { host, port, telnet }) => {
                let mut cfg = NetCfg { host, port, ..Default::default() };
                let s = if telnet {
                    cfg.opts.eol = Eol::Crlf;
                    Stream::Telnet(cfg)
                } else {
                    Stream::Tcp(cfg)
                };
                self.open(&Launch::stream(s), true);
                self.quick.set_text("");
            }
            Some(Quick::Ftp(cfg)) => {
                self.open(&Launch::ftp(cfg), true);
                self.quick.set_text("");
            }
            Some(Quick::Mosh(target)) => {
                if !term::found("mosh") {
                    return self.toast("mosh is not installed (package: mosh)");
                }
                self.open(&Launch::mosh(vec!["--".into(), target.clone()], &target), true);
                self.quick.set_text("");
            }
            Some(Quick::Adb(serial)) => {
                self.open(&Launch::adb(&serial, "", false), true);
                self.quick.set_text("");
            }
            None => self.toast("Use user@host[:port], ssh <args>, mosh host, /dev/ttyX[@baud], telnet://host, tcp://host:port, ftp://host, adb://serial"),
        }
    }
}

/// Terminal that has the keyboard focus.
fn focused(win: &adw::ApplicationWindow) -> Option<crate::vt::View> {
    gtk::prelude::GtkWindowExt::focus(win)
        .and_then(|f| f.ancestor(crate::vt::View::static_type()))
        .and_then(|w| w.downcast::<crate::vt::View>().ok())
}

/// Bar shown while multi-exec is on: selection count and a line to send.
fn multi_bar() -> gtk::Box {
    let title = gtk::Label::new(Some("● Multi-exec"));
    title.add_css_class("heading");
    title.add_css_class("tint-red");
    let count = gtk::Label::new(None);
    count.add_css_class("dim-label");
    let entry = gtk::Entry::new();
    entry.set_hexpand(true);
    entry.set_placeholder_text(Some("Send a line to the selected panes (Enter). Typing in a selected pane goes to all of them."));
    entry.update_property(&[gtk::accessible::Property::Label("Send to selected panes")]);
    let all = gtk::Button::with_label("All");
    all.set_tooltip_text(Some("Select every pane"));
    let none = gtk::Button::with_label("None");
    let exit = gtk::Button::from_icon_name("window-close-symbolic");
    exit.add_css_class("flat");
    exit.set_tooltip_text(Some("Leave multi-exec (Ctrl+Shift+M)"));
    exit.set_action_name(Some("win.multi-exec"));
    all.connect_clicked(|_| multi::select_all(true));
    none.connect_clicked(|_| multi::select_all(false));
    let fire = |e: &gtk::Entry| {
        let line = format!("{}\r", e.text());
        for t in multi::targets() {
            multi::type_into(&t, &line);
        }
        e.set_text("");
    };
    entry.connect_activate(fire);
    let send = gtk::Button::with_label("Send");
    let e = entry.clone();
    send.connect_clicked(move |_| fire(&e));

    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    bar.add_css_class("multibar");
    for w in [title.upcast_ref::<gtk::Widget>(), count.upcast_ref(), entry.upcast_ref(), send.upcast_ref(), all.upcast_ref(), none.upcast_ref(), exit.upcast_ref()] {
        bar.append(w);
    }
    let sync = {
        let (b, c) = (bar.downgrade(), count.downgrade());
        move || {
            let (Some(b), Some(c)) = (b.upgrade(), c.upgrade()) else { return };
            b.set_visible(multi::on());
            let n = multi::targets().len();
            c.set_text(&format!("{n} pane{} selected", if n == 1 { "" } else { "s" }));
        }
    };
    sync();
    multi::on_change(sync);
    bar
}

/// Focused terminal, else the last focused one in this window (the focus
/// may be on a dock control), else the first one in the selected tab.
fn active(win: &adw::ApplicationWindow, view: &adw::TabView, last: &glib::WeakRef<crate::vt::View>) -> Option<crate::vt::View> {
    focused(win)
        .or_else(|| last.upgrade().filter(|t| t.root().is_some_and(|r| &r == win.upcast_ref::<gtk::Root>())))
        .or_else(|| view.selected_page().and_then(|p| pane::find(&p.child())))
}

fn open_tab(view: &adw::TabView, state: &Rc<State>, launch: &Launch) {
    // FTP has no terminal; the window shows it in the Files panel.
    if let Some(cfg) = &launch.ftp {
        let arg = serde_json::to_string(&(launch.title.clone(), cfg, launch.keyring)).unwrap_or_default();
        if let Some(w) = view.root().and_downcast::<adw::ApplicationWindow>() {
            let _ = WidgetExt::activate_action(&w, "win.open-ftp", Some(&arg.to_variant()));
        }
        return;
    }
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
    tabs.append(&multi_bar());
    tabs.append(&bar);
    tabs.append(&view);
    view.set_vexpand(true);

    let main = gtk::Stack::new();
    main.set_vexpand(true);
    main.add_named(&welcome::build(), Some("welcome"));
    main.add_named(&tabs, Some("tabs"));
    main.set_visible_child_name("welcome");

    let dock = Rc::new(dock::build(state));
    let last: Rc<glib::WeakRef<crate::vt::View>> = Rc::default();
    let vsplit = gtk::Paned::new(gtk::Orientation::Vertical);
    vsplit.set_start_child(Some(&main));
    vsplit.set_end_child(Some(&dock.widget));
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
    let multi_btn = gtk::ToggleButton::new();
    multi_btn.set_icon_name("view-grid-symbolic");
    multi_btn.set_tooltip_text(Some("Multi-exec: type into several panes at once (Ctrl+Shift+M)"));
    multi_btn.update_property(&[gtk::accessible::Property::Label("Multi-exec")]);
    multi_btn.set_action_name(Some("win.multi-exec"));
    header.pack_start(&multi_btn);
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
    let (w, v, l) = (win.downgrade(), view.downgrade(), last.clone());
    dock.commands.set_target(move || active(&w.upgrade()?, &v.upgrade()?, &l));
    let (w, v, l) = (win.downgrade(), view.downgrade(), last.clone());
    dock.logs.set_target(move || active(&w.upgrade()?, &v.upgrade()?, &l));

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
        dock: dock.clone(),
        vsplit: vsplit.clone(),
        last: last.clone(),
        status,
        quick: quick.clone(),
        toasts,
        state: state.clone(),
        pick: open_cb,
    };
    add_actions(&ui);
    win.add_controller(dock.commands.keys());
    win.add_controller(ui.sidebar.macros.keys());
    let (w, v, l) = (win.downgrade(), view.downgrade(), last.clone());
    ui.sidebar.macros.set_target(move || active(&w.upgrade()?, &v.upgrade()?, &l));
    if state.cfg.borrow().dock_right {
        ui.place_dock(true);
    }

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
    let (u, logs) = (ui.clone(), Rc::downgrade(&dock.logs));
    win.connect_focus_widget_notify(move |w| {
        if let Some(t) = focused(w) {
            // A different pane: show its command page, if one is made for it.
            if u.last.upgrade().as_ref() != Some(&t) {
                if let Some(l) = u.state.launch(&t) {
                    u.dock.commands.follow(&l.title, &l.target);
                }
            }
            u.last.set(Some(&t));
        }
        u.refresh_status();
        if let Some(l) = logs.upgrade() {
            l.sync();
        }
    });

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

    // The Files panel follows the focused SSH tab's directory.
    let (w, files, st) = (win.downgrade(), ui.sidebar.files.clone(), Rc::downgrade(state));
    state.on_cwd(move |t| {
        let (Some(win), Some(st)) = (w.upgrade(), st.upgrade()) else { return };
        let focused = gtk::prelude::GtkWindowExt::focus(&win)
            .and_then(|f| f.ancestor(crate::vt::View::static_type()))
            .and_then(|w| w.downcast::<crate::vt::View>().ok());
        if focused.as_ref() != Some(t) {
            return;
        }
        if let (Some(l), Some(uri)) = (st.launch(t), t.current_directory_uri()) {
            files.follow(&l, &uri);
        }
    });

    if let Some(msg) = state.notice.take() {
        ui.toast(&msg);
    }
    ui.refresh_status();
    win.present();
    view
}

fn fill_themes(menu: &gio::Menu, state: &State) {
    menu.remove_all();
    let names = gio::Menu::new();
    for t in state.themes() {
        let item = gio::MenuItem::new(Some(&t.name), None);
        item.set_action_and_target_value(Some("win.theme"), Some(&t.name.to_variant()));
        names.append_item(&item);
    }
    menu.append_section(None, &names);
    let edit = gio::Menu::new();
    edit.append(Some("Edit Theme…"), Some("win.edit-theme"));
    menu.append_section(None, &edit);
}

fn menu_model(state: &Rc<State>) -> gio::Menu {
    let panes = gio::Menu::new();
    panes.append(Some("Split Right"), Some("win.split-right"));
    panes.append(Some("Split Down"), Some("win.split-down"));
    panes.append(Some("Close Pane"), Some("win.close-pane"));
    panes.append(Some("Plot Values"), Some("win.plot"));

    let view = gio::Menu::new();
    view.append(Some("Toggle Sidebar"), Some("win.toggle-sidebar"));
    view.append(Some("Toggle Bottom Panel"), Some("win.toggle-dock"));
    view.append(Some("Panel on Right"), Some("win.dock-right"));
    view.append(Some("Multi-Exec"), Some("win.multi-exec"));

    let sess = gio::Menu::new();
    sess.append(Some("Find Session…"), Some("win.find-session"));
    sess.append(Some("Import Sessions…"), Some("win.import"));
    sess.append(Some("Export Sessions…"), Some("win.export"));
    sess.append(Some("Restore Tabs on Start"), Some("win.restore-tabs"));

    let themes = gio::Menu::new();
    fill_themes(&themes, state);
    // User themes come and go (editor, files changed on disk).
    let (w, st) = (themes.downgrade(), Rc::downgrade(state));
    state.on_themes(move || {
        if let (Some(m), Some(s)) = (w.upgrade(), st.upgrade()) {
            fill_themes(&m, &s);
        }
    });

    let menu = gio::Menu::new();
    menu.append_section(None, &sess);
    menu.append_section(None, &panes);
    menu.append_section(None, &view);
    menu.append_submenu(Some("Theme"), &themes);
    let app = gio::Menu::new();
    app.append(Some("Command Palette…"), Some("win.palette"));
    app.append(Some("Preferences…"), Some("win.preferences"));
    menu.append_section(None, &app);
    let rescue = gio::Menu::new();
    rescue.append(Some("Release All Connections…"), Some("win.release-all"));
    menu.append_section(None, &rescue);
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
    F: Fn(&Ui, &crate::vt::View) + 'static,
{
    action(ui, name, None, move |u, _| {
        if let Some(t) = u.active() {
            f(u, &t);
        }
    });
}

/// Every window action with its shortcut, and the themes.
fn command_palette(u: &Ui) {
    let map = crate::keybindings::load();
    let mut items = Vec::new();
    let mut targets: Vec<(String, Option<glib::Variant>)> = Vec::new();
    for (action, label, _) in crate::keybindings::ACTIONS {
        // Leave out actions that cannot run now (and the palette itself).
        let name = action.trim_start_matches("win.");
        if name == "palette" || !u.win.lookup_action(name).is_some_and(|a| a.is_enabled()) {
            continue;
        }
        let keys = crate::keybindings::keys(&map, action);
        let detail = keys.first().map(|k| crate::keys::label(k)).unwrap_or_default();
        items.push(palette::Item { label: label.to_string(), icon: "system-run-symbolic", tint: "", detail });
        targets.push((action.to_string(), None));
    }
    let current = u.state.theme().map(|t| t.name).unwrap_or_default();
    for t in u.state.themes() {
        let detail = if t.name == current { "current".to_string() } else { String::new() };
        items.push(palette::Item { label: format!("Theme: {}", t.name), icon: "applications-graphics-symbolic", tint: "tint-magenta", detail });
        targets.push(("win.theme".into(), Some(t.name.to_variant())));
    }
    let win = u.win.clone();
    palette::choose(&u.win, "Commands", "Type a command", items, move |i| {
        if let Some((action, param)) = targets.get(i) {
            let _ = WidgetExt::activate_action(&win, action, param.as_ref());
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
    // New session editor preset to a type: "serial", "telnet" or "tcp".
    action(ui, "new-session", Some(glib::VariantTy::STRING), |u, p| {
        let init = match p.and_then(|v| v.get::<String>()).as_deref() {
            Some("telnet") => {
                let mut cfg = NetCfg::default();
                cfg.opts.eol = Eol::Crlf;
                Entry::Telnet(store::Net { name: "Telnet".into(), cfg, ..Default::default() })
            }
            Some("ftp") => Entry::Ftp(store::FtpEntry { name: "FTP".into(), ..Default::default() }),
            Some("tcp") => Entry::Tcp(store::Net {
                name: "TCP".into(),
                cfg: NetCfg { port: 5760, ..Default::default() },
                ..Default::default()
            }),
            _ => {
                let cfg = editor::default_serial();
                let name = cfg.device.rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or("Serial").to_string();
                Entry::Serial(store::Serial { name, cfg, ..Default::default() })
            }
        };
        editor::open(&u.win, &u.state, editor::Mode::New(Vec::new()), init);
    });
    action(ui, "open-ftp", Some(glib::VariantTy::STRING), |u, p| {
        let parsed = p
            .and_then(|v| v.get::<String>())
            .and_then(|s| serde_json::from_str::<(String, FtpCfg, bool)>(&s).ok());
        if let Some((title, cfg, keyring)) = parsed {
            u.open_ftp(&title, cfg, keyring);
        }
    });
    action(ui, "adb-files", Some(&glib::VariantType::new("(ss)").expect("type")), |u, p| {
        if let Some((serial, name)) = p.and_then(|v| v.get::<(String, String)>()) {
            u.sidebar.files.open_target(Target::Adb(crate::adb::bin(), serial), &format!("{name} · adb"));
            u.sidebar.show("files");
        }
    });
    let me = gio::SimpleAction::new_stateful("multi-exec", None, &multi::on().to_variant());
    let u = ui.clone();
    me.connect_activate(move |_, _| {
        let on = !multi::on();
        // Starting with nothing selected: take the current pane.
        if on && multi::targets().is_empty() {
            if let Some(t) = u.active() {
                multi::select(&t, true);
            }
        }
        multi::set_on(on);
    });
    let w = me.downgrade();
    multi::on_change(move || {
        if let Some(a) = w.upgrade() {
            a.set_state(&multi::on().to_variant());
        }
    });
    ui.win.add_action(&me);
    action(ui, "preferences", None, |u, _| crate::prefs::open(&u.win, &u.state));
    action(ui, "palette", None, |u, _| command_palette(u));
    action(ui, "edit-theme", None, |u, _| crate::themeedit::open(&u.win, &u.state));
    action(ui, "release-all", None, |u, _| u.release());
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
    action(ui, "toggle-dock", None, |u, _| u.dock.widget.set_visible(!u.dock.widget.is_visible()));
    let right = gio::SimpleAction::new_stateful("dock-right", None, &ui.state.cfg.borrow().dock_right.to_variant());
    let u = ui.clone();
    right.connect_activate(move |a, _| {
        let on = !a.state().and_then(|v| v.get::<bool>()).unwrap_or(false);
        a.set_state(&on.to_variant());
        u.state.edit_cfg(|c| c.dock_right = on);
        u.place_dock(on);
    });
    ui.win.add_action(&right);

    on_term(ui, "split-right", |u, t| {
        pane::split(&u.state, t, gtk::Orientation::Horizontal);
    });
    on_term(ui, "split-down", |u, t| {
        pane::split(&u.state, t, gtk::Orientation::Vertical);
    });
    on_term(ui, "close-pane", |_, t| pane::close(t));
    on_term(ui, "copy", |_, t| t.copy_clipboard());
    on_term(ui, "paste", |_, t| t.paste_clipboard());
    on_term(ui, "plot", |_, t| pane::toggle_plot(t));
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
    let (w, st) = (theme.downgrade(), Rc::downgrade(&ui.state));
    ui.state.on_themes(move || {
        if let (Some(a), Some(s)) = (w.upgrade(), st.upgrade()) {
            a.set_state(&s.theme().map(|t| t.name).unwrap_or_default().to_variant());
        }
    });
    ui.win.add_action(&theme);
}
