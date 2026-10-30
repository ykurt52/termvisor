// Session and folder editor dialog.

use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;

use crate::state::State;
use crate::store::{self, Entry, Folder, Shell, Ssh, SshOpts};

const COLORS: &[&str] = &["Default", "Blue", "Green", "Yellow", "Red", "Magenta", "Cyan"];
const TRISTATE: &[&str] = &["Inherit", "On", "Off"];
const KINDS: &[&str] = &["SSH", "Shell"];

/// Where the result goes.
pub enum Mode {
    /// Append to the folder at this path (root when empty).
    New(Vec<usize>),
    /// Replace the entry at this path.
    Edit(Vec<usize>),
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Ssh,
    Shell,
    Folder,
}

fn entry_row(title: &str) -> adw::EntryRow {
    let r = adw::EntryRow::new();
    r.set_title(title);
    r
}

fn combo(title: &str, items: &[&str]) -> adw::ComboRow {
    let r = adw::ComboRow::new();
    r.set_title(title);
    r.set_model(Some(&gtk::StringList::new(items)));
    r
}

/// Trimmed text, None when empty.
fn text(r: &adw::EntryRow) -> Option<String> {
    let t = r.text().trim().to_string();
    (!t.is_empty()).then_some(t)
}

/// SSH option rows, shared by sessions and folder defaults.
#[derive(Clone)]
struct OptRows {
    port: adw::EntryRow,
    user: adw::EntryRow,
    key: adw::EntryRow,
    jump: adw::EntryRow,
    x11: adw::ComboRow,
    args: adw::EntryRow,
}

impl OptRows {
    fn new() -> Self {
        let rows = Self {
            port: entry_row("Port"),
            user: entry_row("User"),
            key: entry_row("Identity File"),
            jump: entry_row("Jump Host"),
            x11: combo("X11 Forwarding", TRISTATE),
            args: entry_row("Extra Arguments"),
        };
        rows.port.set_input_purpose(gtk::InputPurpose::Digits);

        let pick = gtk::Button::from_icon_name("document-open-symbolic");
        pick.add_css_class("flat");
        pick.set_valign(gtk::Align::Center);
        pick.set_tooltip_text(Some("Choose File"));
        let key = rows.key.clone();
        pick.connect_clicked(move |b| {
            let win = b.root().and_downcast::<gtk::Window>();
            let key = key.clone();
            gtk::FileDialog::new().open(win.as_ref(), None::<&gio::Cancellable>, move |res| {
                if let Some(p) = res.ok().and_then(|f| f.path()) {
                    key.set_text(&p.to_string_lossy());
                }
            });
        });
        rows.key.add_suffix(&pick);
        rows
    }

    fn add_to(&self, g: &adw::PreferencesGroup) {
        g.add(&self.port);
        g.add(&self.user);
        g.add(&self.key);
        g.add(&self.jump);
        g.add(&self.x11);
        g.add(&self.args);
    }

    fn texts(&self) -> [&adw::EntryRow; 5] {
        [&self.port, &self.user, &self.key, &self.jump, &self.args]
    }

    fn set(&self, o: &SshOpts) {
        self.port.set_text(&o.port.map(|p| p.to_string()).unwrap_or_default());
        self.user.set_text(o.user.as_deref().unwrap_or(""));
        self.key.set_text(o.key.as_deref().unwrap_or(""));
        self.jump.set_text(o.jump.as_deref().unwrap_or(""));
        self.x11.set_selected(match o.x11 {
            None => 0,
            Some(true) => 1,
            Some(false) => 2,
        });
        self.args.set_text(&o.args.as_ref().map(|a| a.join(" ")).unwrap_or_default());
    }

    fn get(&self) -> Result<SshOpts, String> {
        let port = match text(&self.port) {
            None => None,
            Some(t) => match t.parse::<u16>() {
                Ok(p) if p > 0 => Some(p),
                _ => return Err("Port must be 1-65535".into()),
            },
        };
        let args = text(&self.args).map(|t| t.split_whitespace().map(str::to_string).collect());
        Ok(SshOpts {
            port,
            user: text(&self.user),
            key: text(&self.key),
            jump: text(&self.jump),
            x11: match self.x11.selected() {
                1 => Some(true),
                2 => Some(false),
                _ => None,
            },
            args,
        })
    }
}

#[derive(Clone)]
struct Form {
    kind: adw::ComboRow,
    name: adw::EntryRow,
    color: adw::ComboRow,
    favorite: adw::SwitchRow,
    conn: adw::PreferencesGroup,
    host: adw::EntryRow,
    opts: OptRows,
    shell: adw::PreferencesGroup,
    dir: adw::EntryRow,
    folder: bool,
}

impl Form {
    fn kind(&self) -> Kind {
        if self.folder {
            Kind::Folder
        } else if self.kind.selected() == 1 {
            Kind::Shell
        } else {
            Kind::Ssh
        }
    }

    fn sync(&self) {
        let k = self.kind();
        self.host.set_visible(k == Kind::Ssh);
        self.conn.set_visible(k != Kind::Shell);
        self.conn.set_title(if k == Kind::Folder { "Defaults for Sessions Inside" } else { "Connection" });
        self.shell.set_visible(k == Kind::Shell);
        self.favorite.set_visible(k != Kind::Folder);
    }

    fn load(&self, e: &Entry) {
        self.name.set_text(e.name());
        let color = e
            .color()
            .and_then(|c| COLORS.iter().position(|x| x.eq_ignore_ascii_case(c)))
            .unwrap_or(0);
        self.color.set_selected(color as u32);
        self.favorite.set_active(e.favorite());
        match e {
            Entry::Ssh(s) => {
                self.kind.set_selected(0);
                self.host.set_text(&s.host);
                self.opts.set(&s.opts);
            }
            Entry::Shell(s) => {
                self.kind.set_selected(1);
                self.dir.set_text(s.dir.as_deref().unwrap_or(""));
            }
            Entry::Folder(f) => self.opts.set(&f.defaults),
        }
    }

    fn build(&self) -> Result<Entry, String> {
        let name = text(&self.name).ok_or("Name is required")?;
        let color = match self.color.selected() as usize {
            0 => None,
            i => COLORS.get(i).map(|c| c.to_lowercase()),
        };
        let favorite = self.favorite.is_active();
        let e = match self.kind() {
            Kind::Ssh => Entry::Ssh(Ssh {
                name,
                host: text(&self.host).ok_or("Host is required")?,
                opts: self.opts.get()?,
                color,
                favorite,
            }),
            Kind::Shell => Entry::Shell(Shell { name, dir: text(&self.dir), color, favorite }),
            Kind::Folder => Entry::Folder(Folder {
                name,
                color,
                defaults: self.opts.get()?,
                children: Vec::new(),
            }),
        };
        // Same checks as launch time, so bad values never reach the file.
        if !matches!(e, Entry::Folder(_)) {
            store::resolve(std::slice::from_ref(&e), &[0])?;
        }
        Ok(e)
    }
}

/// Opens the editor for `init` and saves into `state` on Save.
pub fn open(parent: &impl IsA<gtk::Widget>, state: &Rc<State>, mode: Mode, init: Entry) {
    let folder = matches!(init, Entry::Folder(_));
    let editing = matches!(mode, Mode::Edit(_));

    let general = adw::PreferencesGroup::new();
    let form = Form {
        kind: combo("Type", KINDS),
        name: entry_row("Name"),
        color: combo("Color", COLORS),
        favorite: adw::SwitchRow::new(),
        conn: adw::PreferencesGroup::new(),
        host: entry_row("Host"),
        opts: OptRows::new(),
        shell: adw::PreferencesGroup::new(),
        dir: entry_row("Working Directory"),
        folder,
    };
    form.conn.add(&form.host);
    form.opts.add_to(&form.conn);
    form.favorite.set_title("Favorite");
    form.shell.set_title("Shell");
    form.shell.add(&form.dir);

    general.add(&form.name);
    if !folder {
        general.add(&form.kind);
        form.kind.set_sensitive(!editing);
    }
    general.add(&form.color);
    general.add(&form.favorite);

    let page = adw::PreferencesPage::new();
    page.add(&general);
    page.add(&form.conn);
    page.add(&form.shell);

    form.load(&init);
    form.sync();
    let f = form.clone();
    form.kind.connect_selected_notify(move |_| f.sync());

    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&page));

    let cancel = gtk::Button::with_label("Cancel");
    let save = gtk::Button::with_label("Save");
    save.add_css_class("suggested-action");
    let header = adw::HeaderBar::new();
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    header.pack_start(&cancel);
    header.pack_end(&save);

    let body = adw::ToolbarView::new();
    body.add_top_bar(&header);
    body.set_content(Some(&toasts));

    let what = if folder { "Folder" } else { "Session" };
    let dlg = adw::Dialog::new();
    dlg.set_title(&format!("{} {what}", if editing { "Edit" } else { "New" }));
    dlg.set_content_width(480);
    dlg.set_content_height(620);
    dlg.set_child(Some(&body));

    let d = dlg.clone();
    cancel.connect_clicked(move |_| {
        d.close();
    });

    let d = dlg.clone();
    let st = state.clone();
    let mode = Rc::new(mode);
    let fm = form.clone();
    let submit = Rc::new(move || {
        let res = fm.build().and_then(|e| {
            st.edit_sessions(|s| match &*mode {
                Mode::New(dir) => s.insert(dir, None, e),
                Mode::Edit(path) => s.replace(path, e),
            })
        });
        match res {
            Ok(()) => {
                d.close();
            }
            Err(e) => toasts.add_toast(adw::Toast::new(&e)),
        }
    });
    let s = submit.clone();
    save.connect_clicked(move |_| s());
    // Enter in any text row saves.
    let mut texts = vec![&form.name, &form.host, &form.dir];
    texts.extend(form.opts.texts());
    for r in texts {
        let s = submit.clone();
        r.connect_entry_activated(move |_| s());
    }

    dlg.present(Some(parent));
}
