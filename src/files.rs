// Files panel: SFTP browser bound to the active SSH tab.
//
// The panel follows the last focused SSH tab and connects through its
// shared connection when the panel is first shown. Browsing runs on one
// worker thread; uploads and downloads go to the transfer queue.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{mpsc, Arc};
use std::thread;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::remote::Target;
use crate::sftp::{self, DirEntry};
use crate::state::State;
use crate::term::{self, Launch};
use crate::transfers::{Cmd, Dir};

enum Req {
    Home,
    List(String),
    Mkdir(String),
    Remove(Vec<String>),
    Rename(String, String),
    Chmod(String, u32),
}

enum Rep {
    Home(String),
    Listed(String, Vec<DirEntry>),
    Changed,
    Failed(String),
}

/// Browsing worker: one SFTP session, one request at a time.
fn worker(cmd: Cmd) -> (mpsc::Sender<Req>, async_channel::Receiver<Rep>) {
    let (tx, rx) = mpsc::channel::<Req>();
    let (rtx, rrx) = async_channel::unbounded();
    let spawned = thread::Builder::new().name("sftp-browse".into()).spawn(move || {
        let mut s = match cmd.open() {
            Ok(s) => s,
            Err(e) => {
                let _ = rtx.send_blocking(Rep::Failed(e));
                return;
            }
        };
        let _ = rtx.send_blocking(match s.realpath(".") {
            Ok(h) => Rep::Home(h),
            Err(e) => Rep::Failed(e),
        });
        while let Ok(r) = rx.recv() {
            let rep = match r {
                Req::Home => s.realpath(".").map(Rep::Home),
                Req::List(p) => s.list(&p).map(|l| Rep::Listed(p, l)),
                Req::Mkdir(p) => s.mkdir(&p).map(|_| Rep::Changed),
                Req::Remove(ps) => ps.iter().try_for_each(|p| s.remove_all(p)).map(|_| Rep::Changed),
                Req::Rename(a, b) => s.rename(&a, &b).map(|_| Rep::Changed),
                Req::Chmod(p, m) => s.chmod(&p, m).map(|_| Rep::Changed),
            };
            if rtx.send_blocking(rep.unwrap_or_else(Rep::Failed)).is_err() {
                return;
            }
        }
    });
    if let Err(e) = spawned {
        crate::warn(&format!("sftp worker: {e}"));
    }
    (tx, rrx)
}

struct Inner {
    state: Rc<State>,
    target: RefCell<String>,
    cmd: RefCell<Option<Cmd>>,
    req: RefCell<Option<mpsc::Sender<Req>>>,
    cwd: RefCell<String>,
    entries: RefCell<Vec<DirEntry>>,
    store: gio::ListStore,
    sel: gtk::MultiSelection,
    hidden: Cell<bool>,
    page: gtk::Stack,
    title: gtk::Label,
    path: gtk::Entry,
    info: gtk::Label,
    /// Watches for files opened with "Edit in Local Editor".
    edits: RefCell<Vec<gio::FileMonitor>>,
    /// Go where the SSH tab's shell goes (needs OSC 7 from the remote shell).
    follow: Cell<bool>,
}

#[derive(Clone)]
pub struct Files {
    pub widget: gtk::Stack,
    inner: Rc<Inner>,
}

fn entry_of(obj: Option<glib::Object>) -> Option<DirEntry> {
    let b = obj?.downcast::<glib::BoxedAnyObject>().ok()?;
    let e = b.try_borrow::<DirEntry>().ok()?;
    Some(e.clone())
}

impl Inner {
    fn say(&self, text: &str, error: bool) {
        self.info.set_text(text);
        self.info.set_tooltip_text(Some(text));
        if error {
            self.info.add_css_class("tint-red");
        } else {
            self.info.remove_css_class("tint-red");
        }
    }

    fn send(self: &Rc<Self>, r: Req) {
        let ok = self.req.borrow().as_ref().is_some_and(|tx| tx.send(r).is_ok());
        if !ok {
            self.req.take();
            self.say("not connected; press refresh to reconnect", true);
        }
    }

    fn cd(self: &Rc<Self>, path: &str) {
        self.send(Req::List(path.to_string()));
    }

    fn refresh(self: &Rc<Self>) {
        if self.req.borrow().is_none() {
            self.connect();
            return;
        }
        let cwd = self.cwd.borrow().clone();
        self.cd(&cwd);
    }

    fn fill(&self) {
        self.store.remove_all();
        let hidden = self.hidden.get();
        for e in self.entries.borrow().iter().filter(|e| hidden || !e.name.starts_with('.')) {
            self.store.append(&glib::BoxedAnyObject::new(e.clone()));
        }
    }

    fn selected(&self) -> Vec<DirEntry> {
        let bits = self.sel.selection();
        (0..bits.size())
            .filter_map(|i| entry_of(self.sel.item(bits.nth(i as u32))))
            .collect()
    }

    fn remote(&self, name: &str) -> String {
        sftp::join(&self.cwd.borrow(), name)
    }

    /// Starts the browsing worker for the current target.
    fn connect(self: &Rc<Self>) {
        let Some(cmd) = self.cmd.borrow().clone() else { return };
        let (tx, rx) = worker(cmd);
        self.req.replace(Some(tx));
        self.say("connecting…", false);
        let weak = Rc::downgrade(self);
        let target = self.target.borrow().clone();
        glib::spawn_future_local(async move {
            while let Ok(rep) = rx.recv().await {
                let Some(me) = weak.upgrade() else { break };
                // A newer target replaced this worker.
                if *me.target.borrow() != target {
                    break;
                }
                match rep {
                    Rep::Home(h) => me.cd(&h),
                    Rep::Listed(p, list) => {
                        let n = list.len();
                        me.path.set_text(&p);
                        me.cwd.replace(p);
                        me.entries.replace(list);
                        me.fill();
                        me.say(&format!("{n} items"), false);
                    }
                    Rep::Changed => me.refresh(),
                    Rep::Failed(e) => me.say(&e, true),
                }
            }
        });
    }

    fn queue(&self, dir: Dir, local: PathBuf, remote: String) {
        if let Some(cmd) = self.cmd.borrow().clone() {
            self.state.xfers.add(dir, local, remote, cmd, false);
        }
    }

    fn upload(&self, paths: Vec<PathBuf>) {
        for p in paths {
            let Some(name) = p.file_name().map(|n| n.to_string_lossy().into_owned()) else { continue };
            let remote = self.remote(&name);
            self.queue(Dir::Up, p, remote);
        }
    }

    /// Opens a downloaded copy in the default app and uploads it on save.
    fn watch_edit(self: &Rc<Self>, local: PathBuf, remote: String) {
        let name = local.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let file = gio::File::for_path(&local);
        if let Err(e) = gio::AppInfo::launch_default_for_uri(&file.uri(), None::<&gio::AppLaunchContext>) {
            self.say(&format!("no app opens {name}: {e}"), true);
            return;
        }
        let mon = match file.monitor_file(gio::FileMonitorFlags::WATCH_MOVES, None::<&gio::Cancellable>) {
            Ok(m) => m,
            Err(e) => {
                self.say(&format!("cannot watch {name}: {e}"), true);
                return;
            }
        };
        // Editors save in bursts (write, rename, chmod); upload once it settles.
        let pending: Rc<RefCell<Option<glib::SourceId>>> = Rc::default();
        let weak = Rc::downgrade(self);
        mon.connect_changed(move |_, _, _, ev| {
            use gio::FileMonitorEvent as E;
            if !matches!(ev, E::ChangesDoneHint | E::Created | E::Renamed | E::MovedIn) {
                return;
            }
            if let Some(id) = pending.take() {
                id.remove();
            }
            let (w, l, r, p) = (weak.clone(), local.clone(), remote.clone(), pending.clone());
            let id = glib::timeout_add_local_once(std::time::Duration::from_millis(700), move || {
                p.take();
                let Some(me) = w.upgrade() else { return };
                let cmd = me.cmd.borrow().clone();
                if let Some(cmd) = cmd {
                    me.state.xfers.add(Dir::Up, l, r.clone(), cmd, true);
                    me.say(&format!("uploading saved {}", r.rsplit('/').next().unwrap_or(&r)), false);
                }
            });
            pending.replace(Some(id));
        });
        self.edits.borrow_mut().push(mon);
        self.say(&format!("editing {name}; every save uploads it"), false);
    }

    fn download(&self, items: Vec<DirEntry>, to: PathBuf) {
        for e in items {
            let remote = self.remote(&e.name);
            self.queue(Dir::Down, to.join(&e.name), remote);
        }
    }
}

/// Private folder for files opened in a local editor.
fn edit_dir() -> Result<PathBuf, String> {
    use std::os::unix::fs::DirBuilderExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let dir = glib::user_runtime_dir()
        .join("termvisor")
        .join("edit")
        .join(format!("{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(dir)
}

/// Remote path from an OSC 7 URI "file://host/some%20dir".
fn uri_path(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    let i = rest.find('/')?;
    glib::Uri::unescape_string(&rest[i..], None::<&str>).map(|s| s.to_string())
}

pub fn downloads_dir() -> PathBuf {
    glib::user_special_dir(glib::UserDirectory::Downloads).unwrap_or_else(glib::home_dir)
}

/// Asks for one line of text; `done` gets it on OK.
fn ask(parent: &gtk::Widget, heading: &str, initial: &str, ok: &str, done: impl Fn(String) + 'static) {
    let d = adw::AlertDialog::new(Some(heading), None);
    let e = gtk::Entry::new();
    e.set_text(initial);
    e.set_activates_default(true);
    d.set_extra_child(Some(&e));
    d.add_responses(&[("cancel", "Cancel"), ("ok", ok)]);
    d.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
    d.set_default_response(Some("ok"));
    d.set_close_response("cancel");
    d.connect_response(None, move |_, r| {
        if r == "ok" {
            done(e.text().trim().to_string());
        }
    });
    d.present(Some(parent));
}

fn label_column(title: &str, expand: bool, text: fn(&DirEntry) -> String) -> gtk::ColumnViewColumn {
    let f = gtk::SignalListItemFactory::new();
    f.connect_setup(|_, o| {
        if let Some(li) = o.downcast_ref::<gtk::ListItem>() {
            let l = gtk::Label::new(None);
            l.set_xalign(0.0);
            l.add_css_class("dim-label");
            li.set_child(Some(&l));
        }
    });
    f.connect_bind(move |_, o| {
        let Some(li) = o.downcast_ref::<gtk::ListItem>() else { return };
        let (Some(l), Some(e)) = (li.child().and_downcast::<gtk::Label>(), entry_of(li.item())) else { return };
        l.set_text(&text(&e));
    });
    let c = gtk::ColumnViewColumn::new(Some(title), Some(f));
    c.set_expand(expand);
    c.set_resizable(true);
    c
}

fn name_column() -> gtk::ColumnViewColumn {
    let f = gtk::SignalListItemFactory::new();
    f.connect_setup(|_, o| {
        if let Some(li) = o.downcast_ref::<gtk::ListItem>() {
            let b = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            b.append(&gtk::Image::new());
            let l = gtk::Label::new(None);
            l.set_xalign(0.0);
            l.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            b.append(&l);
            li.set_child(Some(&b));
        }
    });
    f.connect_bind(|_, o| {
        let Some(li) = o.downcast_ref::<gtk::ListItem>() else { return };
        let Some(e) = entry_of(li.item()) else { return };
        let Some(icon) = li.child().and_then(|b| b.first_child()).and_downcast::<gtk::Image>() else { return };
        let Some(label) = icon.next_sibling().and_downcast::<gtk::Label>() else { return };
        let (name, tint) = if e.attrs.is_dir() {
            ("folder-symbolic", "tint-yellow")
        } else if e.attrs.is_link() {
            ("insert-link-symbolic", "tint-cyan")
        } else {
            ("text-x-generic-symbolic", "tint-blue")
        };
        icon.set_icon_name(Some(name));
        icon.set_css_classes(&[tint]);
        label.set_text(&e.name);
    });
    let c = gtk::ColumnViewColumn::new(Some("Name"), Some(f));
    c.set_expand(true);
    c.set_resizable(true);
    c
}

fn size_text(e: &DirEntry) -> String {
    if e.attrs.is_dir() {
        String::new()
    } else {
        e.attrs.size.map(|s| glib::format_size(s).to_string()).unwrap_or_default()
    }
}

fn date_text(e: &DirEntry) -> String {
    e.attrs
        .mtime
        .and_then(|t| glib::DateTime::from_unix_local(i64::from(t)).ok())
        .and_then(|d| d.format("%Y-%m-%d %H:%M").ok())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

fn tool(icon: &str, tip: &str, action: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.add_css_class("flat");
    b.set_tooltip_text(Some(tip));
    b.set_action_name(Some(action));
    b
}

fn context_menu(dir_selected: bool) -> gio::Menu {
    let m = gio::Menu::new();
    let a = gio::Menu::new();
    a.append(Some("Download"), Some("files.download"));
    a.append(Some("Download To…"), Some("files.download-to"));
    a.append(Some("Edit in Local Editor"), Some("files.edit"));
    if dir_selected {
        a.append(Some("Open"), Some("files.open"));
    }
    m.append_section(None, &a);
    let b = gio::Menu::new();
    b.append(Some("Rename…"), Some("files.rename"));
    b.append(Some("Permissions…"), Some("files.chmod"));
    b.append(Some("Copy Path"), Some("files.copy-path"));
    m.append_section(None, &b);
    let c = gio::Menu::new();
    c.append(Some("New Folder…"), Some("files.mkdir"));
    c.append(Some("Upload Files…"), Some("files.upload"));
    c.append(Some("Upload Folder…"), Some("files.upload-folder"));
    m.append_section(None, &c);
    let d = gio::Menu::new();
    d.append(Some("Delete…"), Some("files.delete"));
    m.append_section(None, &d);
    m
}

impl Files {
    pub fn new(state: &Rc<State>) -> Self {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let sel = gtk::MultiSelection::new(Some(store.clone()));
        let view = gtk::ColumnView::new(Some(sel.clone()));
        view.add_css_class("data-table");
        view.append_column(&name_column());
        view.append_column(&label_column("Size", false, size_text));
        view.append_column(&label_column("Modified", false, date_text));
        view.append_column(&label_column("Mode", false, |e| e.attrs.mode_text()));

        let title = gtk::Label::new(None);
        title.set_xalign(0.0);
        title.add_css_class("heading");
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.set_margin_start(8);
        let path = gtk::Entry::new();
        path.set_margin_start(6);
        path.set_margin_end(6);
        let info = gtk::Label::new(None);
        info.set_xalign(0.0);
        info.add_css_class("dim-label");
        info.set_ellipsize(gtk::pango::EllipsizeMode::End);
        info.set_margin_start(8);
        info.set_margin_bottom(4);

        let tools = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        tools.set_margin_start(4);
        for (icon, tip, action) in [
            ("go-up-symbolic", "Parent Folder", "files.up"),
            ("go-home-symbolic", "Home", "files.home"),
            ("view-refresh-symbolic", "Refresh / Reconnect", "files.refresh"),
            ("folder-new-symbolic", "New Folder", "files.mkdir"),
            ("document-send-symbolic", "Upload Files", "files.upload"),
            ("folder-download-symbolic", "Download Selected", "files.download"),
        ] {
            tools.append(&tool(icon, tip, action));
        }
        let follow = gtk::ToggleButton::new();
        follow.set_icon_name("mark-location-symbolic");
        follow.add_css_class("flat");
        follow.set_active(true);
        follow.set_tooltip_text(Some("Follow the terminal's folder (remote shell must send OSC 7)"));
        tools.append(&follow);
        let hidden = gtk::ToggleButton::new();
        hidden.set_icon_name("view-reveal-symbolic");
        hidden.add_css_class("flat");
        hidden.set_tooltip_text(Some("Show Hidden Files"));
        tools.append(&hidden);

        let scroll = gtk::ScrolledWindow::new();
        scroll.set_vexpand(true);
        scroll.set_child(Some(&view));

        let browse = gtk::Box::new(gtk::Orientation::Vertical, 4);
        browse.append(&title);
        browse.append(&tools);
        browse.append(&path);
        browse.append(&scroll);
        browse.append(&info);

        let empty = adw::StatusPage::new();
        empty.set_icon_name(Some("folder-symbolic"));
        empty.set_description(Some("Open or focus an SSH tab to browse its files here."));
        empty.add_css_class("compact");

        let page = gtk::Stack::new();
        page.set_vexpand(true);
        page.add_named(&empty, Some("empty"));
        page.add_named(&browse, Some("browse"));
        page.set_visible_child_name("empty");

        let inner = Rc::new(Inner {
            state: state.clone(),
            target: RefCell::default(),
            cmd: RefCell::default(),
            req: RefCell::default(),
            cwd: RefCell::new("/".into()),
            entries: RefCell::default(),
            store,
            sel,
            hidden: Cell::new(false),
            page: page.clone(),
            title,
            path: path.clone(),
            info,
            edits: RefCell::default(),
            follow: Cell::new(true),
        });

        let me = inner.clone();
        follow.connect_toggled(move |b| me.follow.set(b.is_active()));
        let me = inner.clone();
        hidden.connect_toggled(move |b| {
            me.hidden.set(b.is_active());
            me.fill();
        });
        let me = inner.clone();
        path.connect_activate(move |e| me.cd(e.text().trim()));

        let me = inner.clone();
        view.connect_activate(move |_, pos| {
            let Some(e) = entry_of(me.sel.item(pos)) else { return };
            if e.attrs.is_dir() {
                let p = me.remote(&e.name);
                me.cd(&p);
            } else {
                me.download(vec![e], downloads_dir());
            }
        });

        // Right-click selects the row under the pointer when it is not
        // part of the selection, then shows the menu.
        let click = gtk::GestureClick::new();
        click.set_button(3);
        let me = inner.clone();
        click.connect_pressed(move |g, _, x, y| {
            let Some(w) = g.widget() else { return };
            let dir = me.selected().iter().any(|e| e.attrs.is_dir());
            let pop = gtk::PopoverMenu::from_model(Some(&context_menu(dir)));
            pop.set_parent(&w);
            pop.set_has_arrow(false);
            pop.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
            pop.connect_closed(|p| {
                let p = p.clone();
                glib::idle_add_local_once(move || p.unparent());
            });
            pop.popup();
        });
        view.add_controller(click);

        // Files dropped from a file manager are uploaded here.
        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let me = inner.clone();
        drop.connect_drop(move |_, v, _, _| {
            let Ok(list) = v.get::<gdk::FileList>() else { return false };
            if me.cmd.borrow().is_none() {
                return false;
            }
            me.upload(list.files().iter().filter_map(gio::File::path).collect());
            true
        });
        view.add_controller(drop);

        // Connect lazily when the panel is shown.
        let me = inner.clone();
        page.connect_map(move |_| {
            if me.req.borrow().is_none() && me.cmd.borrow().is_some() {
                me.connect();
            }
        });

        // Refresh after uploads into this host finish.
        let me = Rc::downgrade(&inner);
        state.xfers.on_change(move |id| {
            let Some(me) = me.upgrade() else { return };
            let done_up = me
                .state
                .xfers
                .item(id)
                .is_some_and(|i| i.finished() && i.dir == Dir::Up);
            if done_up && me.req.borrow().is_some() {
                me.refresh();
            }
        });

        page.insert_action_group("files", Some(&actions(&inner)));
        Files { widget: page, inner }
    }

    /// Shows `target` (e.g. an FTP server) under `title` and connects.
    pub fn open_target(&self, target: Target, title: &str) {
        let me = &self.inner;
        me.target.replace(target.key());
        me.cmd.replace(Some(Arc::new(target)));
        me.req.take();
        me.entries.borrow_mut().clear();
        me.store.remove_all();
        me.title.set_text(title);
        me.page.set_visible_child_name("browse");
        me.connect();
    }

    /// Goes to the folder an SSH tab's shell reported (OSC 7) when this
    /// panel shows the same host and following is on.
    pub fn follow(&self, launch: &Launch, uri: &str) {
        let me = &self.inner;
        if !me.follow.get() || *me.target.borrow() != launch.target || me.req.borrow().is_none() {
            return;
        }
        if let Some(p) = uri_path(uri) {
            if *me.cwd.borrow() != p {
                me.cd(&p);
            }
        }
    }

    /// Follows `launch` when it is an SSH session on a different host.
    pub fn attach(&self, launch: Option<&Launch>) {
        let Some(l) = launch else { return };
        let Some((argv, env)) = term::sftp_command(l) else { return };
        let me = &self.inner;
        if *me.target.borrow() == l.target {
            return;
        }
        me.target.replace(l.target.clone());
        me.cmd.replace(Some(Arc::new(Target::Sftp((argv, env)))));
        me.req.take();
        me.entries.borrow_mut().clear();
        me.store.remove_all();
        me.title.set_text(&l.target);
        me.page.set_visible_child_name("browse");
        me.say("opens when this panel is shown", false);
        if me.page.is_mapped() {
            me.connect();
        }
    }
}

type Handler = Box<dyn Fn(&Rc<Inner>)>;

fn actions(me: &Rc<Inner>) -> gio::SimpleActionGroup {
    let g = gio::SimpleActionGroup::new();
    let add = |name: &str, f: Handler| {
        let a = gio::SimpleAction::new(name, None);
        let me = me.clone();
        a.connect_activate(move |_, _| f(&me));
        g.add_action(&a);
    };

    add("up", Box::new(|me| {
        let p = sftp::parent(&me.cwd.borrow());
        me.cd(&p);
    }));
    add("home", Box::new(|me| me.send(Req::Home)));
    add("refresh", Box::new(|me| me.refresh()));
    add("open", Box::new(|me| {
        if let Some(e) = me.selected().into_iter().find(|e| e.attrs.is_dir()) {
            let p = me.remote(&e.name);
            me.cd(&p);
        }
    }));
    add("download", Box::new(|me| me.download(me.selected(), downloads_dir())));
    add("edit", Box::new(|me| {
        let Some(e) = me.selected().into_iter().find(|e| !e.attrs.is_dir()) else { return };
        let Some(cmd) = me.cmd.borrow().clone() else { return };
        let dir = match edit_dir() {
            Ok(d) => d,
            Err(err) => return me.say(&err, true),
        };
        let (remote, local) = (me.remote(&e.name), dir.join(&e.name));
        me.say(&format!("opening {}…", e.name), false);
        let (tx, rx) = async_channel::bounded(1);
        let (r, l) = (remote.clone(), local.clone());
        let spawned = thread::Builder::new().name("sftp-edit".into()).spawn(move || {
            let res = cmd.open().and_then(|mut s| {
                let mut f = std::fs::File::create(&l).map_err(|e| e.to_string())?;
                s.download(&r, 0, &mut f, &mut |_| true)
            });
            let _ = tx.send_blocking(res);
        });
        if spawned.is_err() {
            return me.say("cannot start download", true);
        }
        let m = me.clone();
        glib::spawn_future_local(async move {
            match rx.recv().await {
                Ok(Ok(_)) => m.watch_edit(local, remote),
                Ok(Err(err)) => m.say(&err, true),
                Err(_) => {}
            }
        });
    }));
    add("download-to", Box::new(|me| {
        let items = me.selected();
        if items.is_empty() {
            return;
        }
        let win = me.page.root().and_downcast::<gtk::Window>();
        let m = me.clone();
        gtk::FileDialog::new().select_folder(win.as_ref(), None::<&gio::Cancellable>, move |r| {
            if let Some(dir) = r.ok().and_then(|f| f.path()) {
                m.download(items, dir);
            }
        });
    }));
    add("upload", Box::new(|me| {
        let win = me.page.root().and_downcast::<gtk::Window>();
        let m = me.clone();
        gtk::FileDialog::new().open_multiple(win.as_ref(), None::<&gio::Cancellable>, move |r| {
            let Ok(list) = r else { return };
            let paths = (0..list.n_items())
                .filter_map(|i| list.item(i).and_downcast::<gio::File>())
                .filter_map(|f| f.path())
                .collect();
            m.upload(paths);
        });
    }));
    add("upload-folder", Box::new(|me| {
        let win = me.page.root().and_downcast::<gtk::Window>();
        let m = me.clone();
        gtk::FileDialog::new().select_folder(win.as_ref(), None::<&gio::Cancellable>, move |r| {
            if let Some(p) = r.ok().and_then(|f| f.path()) {
                m.upload(vec![p]);
            }
        });
    }));
    add("mkdir", Box::new(|me| {
        let m = me.clone();
        ask(me.page.upcast_ref(), "New Folder", "", "Create", move |name| {
            if !name.is_empty() && !name.contains('/') {
                let p = m.remote(&name);
                m.send(Req::Mkdir(p));
            }
        });
    }));
    add("rename", Box::new(|me| {
        let Some(e) = me.selected().into_iter().next() else { return };
        let (m, old) = (me.clone(), e.name.clone());
        ask(me.page.upcast_ref(), "Rename", &e.name, "Rename", move |name| {
            if !name.is_empty() && !name.contains('/') && name != old {
                let (a, b) = (m.remote(&old), m.remote(&name));
                m.send(Req::Rename(a, b));
            }
        });
    }));
    add("chmod", Box::new(|me| {
        let items = me.selected();
        let Some(first) = items.first() else { return };
        let cur = first.attrs.perm.map(|p| format!("{:o}", p & 0o7777)).unwrap_or_else(|| "644".into());
        let m = me.clone();
        let heading = format!("Permissions (octal) for {} item(s)", items.len());
        ask(me.page.upcast_ref(), &heading, &cur, "Apply", move |text| {
            let Ok(mode) = u32::from_str_radix(&text, 8) else {
                m.say(&format!("'{text}' is not an octal mode"), true);
                return;
            };
            for e in &items {
                m.send(Req::Chmod(m.remote(&e.name), mode));
            }
        });
    }));
    add("copy-path", Box::new(|me| {
        let text: Vec<String> = me.selected().iter().map(|e| me.remote(&e.name)).collect();
        if !text.is_empty() {
            me.page.clipboard().set_text(&text.join("\n"));
        }
    }));
    add("delete", Box::new(|me| {
        let items = me.selected();
        if items.is_empty() {
            return;
        }
        let what = match items.as_slice() {
            [one] => format!("“{}”", one.name),
            many => format!("{} items", many.len()),
        };
        let d = adw::AlertDialog::new(
            Some(&format!("Delete {what}?")),
            Some("Folders are deleted with everything inside. This cannot be undone."),
        );
        d.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
        d.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        d.set_default_response(Some("cancel"));
        d.set_close_response("cancel");
        let m = me.clone();
        d.connect_response(None, move |_, r| {
            if r == "delete" {
                let paths = items.iter().map(|e| m.remote(&e.name)).collect();
                m.send(Req::Remove(paths));
            }
        });
        d.present(Some(&me.page));
    }));
    g
}

#[cfg(test)]
mod tests {
    use super::uri_path;

    #[test]
    fn osc7_uri() {
        assert_eq!(uri_path("file://board/home/root/my%20dir").as_deref(), Some("/home/root/my dir"));
        assert_eq!(uri_path("file:///tmp").as_deref(), Some("/tmp"));
        assert_eq!(uri_path("http://x/y"), None);
    }
}
