// Command panel: pages of up to 50 buttons that send text or bytes to the
// focused pane.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, gio, glib, glib::translate::IntoGlib, graphene};
use vte::prelude::*;

use crate::commands::{self, Button, Ending, Page, MAX_BUTTONS};
use crate::state::State;
use crate::streampane;

/// Shown name and stored value of button colors.
const COLORS: &[(&str, Option<&str>)] = &[
    ("Default", None),
    ("Blue", Some("blue")),
    ("Green", Some("green")),
    ("Yellow", Some("yellow")),
    ("Red", Some("red")),
    ("Magenta", Some("magenta")),
    ("Cyan", Some("cyan")),
];

const TEXT_HINT: &str = "One command per line. Escapes: \\r \\n \\t \\0 \\\\ \\xHH";
const HEX_HINT: &str = "Hex bytes: 55 AA 01, 55AA01 or 0x55,0xAA";

type Target = Box<dyn Fn() -> Option<vte::Terminal>>;

pub struct Panel {
    pub widget: gtk::Box,
    state: Rc<State>,
    page: Cell<usize>,
    names: gtk::StringList,
    pick: gtk::DropDown,
    flow: gtk::FlowBox,
    count: gtk::Label,
    menu: gtk::PopoverMenu,
    /// Button shortcuts of the shown page; added to the window.
    keys: gtk::ShortcutController,
    fkeys: RefCell<Option<gio::SimpleAction>>,
    /// Finds the pane that receives button sends.
    target: RefCell<Option<Target>>,
    /// Set while the page list is rebuilt, so the dropdown signal is ignored.
    syncing: Cell<bool>,
}

pub fn build(state: &Rc<State>) -> Rc<Panel> {
    let names = gtk::StringList::new(&[]);
    let pick = gtk::DropDown::new(Some(names.clone()), None::<gtk::Expression>);
    pick.set_tooltip_text(Some("Command page"));

    let pages = gio::Menu::new();
    pages.append(Some("New Page…"), Some("cmd.page-new"));
    pages.append(Some("Page Settings…"), Some("cmd.page-settings"));
    pages.append(Some("Delete Page"), Some("cmd.page-delete"));
    let keys = gio::Menu::new();
    keys.append(Some("F1–F12 Send Buttons 1–12"), Some("cmd.fkeys"));
    let files = gio::Menu::new();
    files.append(Some("Import…"), Some("cmd.import"));
    files.append(Some("Export…"), Some("cmd.export"));
    let model = gio::Menu::new();
    model.append_section(None, &pages);
    model.append_section(None, &keys);
    model.append_section(None, &files);
    let more = gtk::MenuButton::new();
    more.set_icon_name("view-more-symbolic");
    more.add_css_class("flat");
    more.set_tooltip_text(Some("Pages, import and export"));
    more.set_menu_model(Some(&model));

    let add = gtk::Button::from_icon_name("list-add-symbolic");
    add.add_css_class("flat");
    add.set_tooltip_text(Some("Add a button"));
    add.set_action_name(Some("cmd.add"));

    let count = gtk::Label::new(None);
    count.add_css_class("dim-label");
    let hint = gtk::Label::new(Some("Click sends to the focused pane · right-click edits · drag reorders"));
    hint.add_css_class("dim-label");
    hint.set_hexpand(true);
    hint.set_xalign(1.0);
    hint.set_ellipsize(gtk::pango::EllipsizeMode::End);

    let head = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    head.set_margin_start(6);
    head.set_margin_end(6);
    head.set_margin_top(4);
    head.append(&pick);
    head.append(&more);
    head.append(&add);
    head.append(&count);
    head.append(&hint);

    let flow = gtk::FlowBox::new();
    flow.set_selection_mode(gtk::SelectionMode::None);
    flow.set_max_children_per_line(MAX_BUTTONS as u32);
    flow.set_row_spacing(4);
    flow.set_column_spacing(4);
    flow.set_valign(gtk::Align::Start);
    flow.set_margin_start(6);
    flow.set_margin_end(6);
    flow.set_margin_top(4);
    flow.set_margin_bottom(6);
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
    scroll.set_vexpand(true);
    scroll.set_child(Some(&flow));

    let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
    widget.add_css_class("cmdpanel");
    widget.append(&head);
    widget.append(&scroll);

    // One context menu for all buttons; buttons are rebuilt on every change.
    let menu = gtk::PopoverMenu::from_model(None::<&gio::MenuModel>);
    menu.set_parent(&widget);
    let m = menu.clone();
    widget.connect_destroy(move |_| m.unparent());

    let keys = gtk::ShortcutController::new();
    // Before the terminal, which would take the keys otherwise.
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);

    let p = Rc::new(Panel {
        widget,
        state: state.clone(),
        page: Cell::new(0),
        names,
        pick,
        flow,
        count,
        menu,
        keys,
        fkeys: RefCell::new(None),
        target: RefCell::new(None),
        syncing: Cell::new(false),
    });
    let group = actions(&p);
    p.fkeys.replace(group.lookup_action("fkeys").and_downcast());
    p.widget.insert_action_group("cmd", Some(&group));
    // The panel lives as long as its widget; destroy breaks the cycle.
    let keep = RefCell::new(Some(p.clone()));
    p.widget.connect_destroy(move |_| drop(keep.take()));

    let w = Rc::downgrade(&p);
    p.pick.connect_selected_notify(move |d| {
        let Some(p) = w.upgrade() else { return };
        if !p.syncing.get() {
            p.page.set(d.selected() as usize);
            p.fill();
        }
    });
    let w = Rc::downgrade(&p);
    state.on_commands(move || {
        if let Some(p) = w.upgrade() {
            p.refresh();
        }
    });
    p.refresh();
    p
}

impl Panel {
    /// Sets how the receiving pane is found (normally the focused one).
    pub fn set_target(&self, f: impl Fn() -> Option<vte::Terminal> + 'static) {
        self.target.replace(Some(Box::new(f)));
    }

    /// Shortcuts of the shown page, for the window to handle.
    pub fn keys(&self) -> gtk::ShortcutController {
        self.keys.clone()
    }

    /// Shows the first page made for the session `title` / `target`,
    /// unless the shown page is one of them.
    pub fn follow(&self, title: &str, target: &str) {
        let pages = self.state.commands.borrow();
        if pages.get(self.page.get()).is_some_and(|p| p.matches(title, target)) {
            return;
        }
        let Some(at) = pages.iter().position(|p| p.matches(title, target)) else { return };
        drop(pages);
        self.page.set(at);
        self.refresh();
    }

    fn toast(&self, msg: &str) {
        match self.widget.ancestor(adw::ToastOverlay::static_type()).and_downcast::<adw::ToastOverlay>() {
            Some(t) => t.add_toast(adw::Toast::new(msg)),
            None => crate::warn(msg),
        }
    }

    fn window(&self) -> Option<gtk::Window> {
        self.widget.root().and_downcast()
    }

    fn current(&self) -> Option<Page> {
        self.state.commands.borrow().get(self.page.get()).cloned()
    }

    fn button(&self, i: usize) -> Option<Button> {
        self.current().and_then(|p| p.buttons.get(i).cloned())
    }

    /// Changes the shown page and saves.
    fn edit(&self, f: impl FnOnce(&mut Page)) {
        let at = self.page.get();
        let res = self.state.edit_commands(|pages| {
            if let Some(p) = pages.get_mut(at) {
                f(p);
            }
        });
        if let Err(e) = res {
            self.toast(&e);
        }
    }

    /// Rebuilds the page list and the buttons.
    fn refresh(&self) {
        let names: Vec<String> = self.state.commands.borrow().iter().map(|p| p.name.clone()).collect();
        let at = self.page.get().min(names.len().saturating_sub(1));
        self.page.set(at);
        self.syncing.set(true);
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        self.names.splice(0, self.names.n_items(), &refs);
        self.pick.set_selected(at as u32);
        self.syncing.set(false);
        if let Some(a) = self.fkeys.borrow().as_ref() {
            a.set_state(&self.state.cfg.borrow().command_keys.to_variant());
        }
        self.fill();
    }

    /// Key of button `i`: its own, else F1–F12 for the first 12 when on.
    fn key_of(&self, i: usize, b: &Button) -> Option<String> {
        b.key.clone().or_else(|| (self.state.cfg.borrow().command_keys && i < 12).then(|| format!("F{}", i + 1)))
    }

    /// Rebuilds the shortcuts for `page`.
    fn bind(&self, page: &Page) {
        let old: Vec<gtk::Shortcut> = (0..self.keys.n_items()).filter_map(|i| self.keys.item(i).and_downcast()).collect();
        for s in old {
            self.keys.remove_shortcut(&s);
        }
        // Own keys first, so they win over the F1–F12 defaults.
        let mut keys: Vec<(usize, String)> =
            page.buttons.iter().enumerate().filter_map(|(i, b)| b.key.clone().map(|k| (i, k))).collect();
        keys.extend(
            page.buttons
                .iter()
                .enumerate()
                .filter(|(_, b)| b.key.is_none())
                .filter_map(|(i, b)| self.key_of(i, b).map(|k| (i, k))),
        );
        for (i, key) in keys {
            let Some(trigger) = gtk::ShortcutTrigger::parse_string(&key) else { continue };
            let panel = self.widget.downgrade();
            // Only while a terminal has the focus; dialogs and entries keep their keys.
            let action = gtk::CallbackAction::new(move |w, _| {
                let in_term = w
                    .downcast_ref::<gtk::Window>()
                    .and_then(gtk::prelude::GtkWindowExt::focus)
                    .and_then(|f| f.ancestor(vte::Terminal::static_type()))
                    .is_some();
                match panel.upgrade() {
                    Some(p) if in_term => {
                        let _ = WidgetExt::activate_action(&p, "cmd.send", Some(&(i as u32).to_variant()));
                        glib::Propagation::Stop
                    }
                    _ => glib::Propagation::Proceed,
                }
            });
            self.keys.add_shortcut(gtk::Shortcut::new(Some(trigger), Some(action)));
        }
    }

    fn fill(&self) {
        self.flow.remove_all();
        let page = self.current().unwrap_or_default();
        for (i, b) in page.buttons.iter().enumerate() {
            self.flow.append(&self.make(i, b, self.key_of(i, b)));
        }
        self.bind(&page);
        if page.buttons.len() < MAX_BUTTONS {
            let add = gtk::Button::with_label("+");
            add.add_css_class("cmd");
            add.add_css_class("cmd-new");
            add.set_tooltip_text(Some("Add a button"));
            add.set_focus_on_click(false);
            add.set_action_name(Some("cmd.add"));
            self.flow.append(&add);
        }
        let mut child = self.flow.first_child();
        while let Some(c) = child {
            c.set_focusable(false);
            child = c.next_sibling();
        }
        self.count.set_text(&format!("{}/{MAX_BUTTONS}", page.buttons.len()));
    }

    fn make(&self, i: usize, b: &Button, key: Option<String>) -> gtk::Button {
        let label = if b.label.is_empty() { "(no label)" } else { &b.label };
        let shown = key.as_deref().map(key_label);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        row.append(&gtk::Label::new(Some(label)));
        if let Some(k) = &shown {
            let l = gtk::Label::new(Some(k));
            l.add_css_class("dim-label");
            l.add_css_class("caption");
            row.append(&l);
        }
        let w = gtk::Button::new();
        w.set_child(Some(&row));
        w.update_property(&[gtk::accessible::Property::Label(label)]);
        w.add_css_class("cmd");
        if let Some(c) = &b.color {
            w.add_css_class(&format!("c-{c}"));
        }
        // Keeps the keyboard focus, and so the target, on the terminal.
        w.set_focus_on_click(false);
        let tip = match &shown {
            Some(k) => format!("{}\nShortcut: {k}", preview(b)),
            None => preview(b),
        };
        w.set_tooltip_text(Some(&tip));
        w.set_action_name(Some("cmd.send"));
        w.set_action_target_value(Some(&(i as u32).to_variant()));

        let menu = self.menu.downgrade();
        let panel = self.widget.downgrade();
        let click = gtk::GestureClick::new();
        click.set_button(gdk::BUTTON_SECONDARY);
        click.connect_pressed(move |g, _, x, y| {
            g.set_state(gtk::EventSequenceState::Claimed);
            let (Some(menu), Some(panel), Some(w)) = (menu.upgrade(), panel.upgrade(), g.widget()) else { return };
            let at = w.compute_point(&panel, &graphene::Point::new(x as f32, y as f32)).unwrap_or(graphene::Point::zero());
            menu.set_menu_model(Some(&context_menu(i)));
            menu.set_pointing_to(Some(&gdk::Rectangle::new(at.x() as i32, at.y() as i32, 1, 1)));
            menu.popup();
        });
        w.add_controller(click);

        let drag = gtk::DragSource::new();
        drag.set_actions(gdk::DragAction::MOVE);
        drag.connect_prepare(move |_, _, _| Some(gdk::ContentProvider::for_value(&(i as u32).to_value())));
        w.add_controller(drag);

        let drop = gtk::DropTarget::new(u32::static_type(), gdk::DragAction::MOVE);
        let flow = self.flow.downgrade();
        drop.connect_drop(move |_, v, _, _| {
            let (Ok(from), Some(flow)) = (v.get::<u32>(), flow.upgrade()) else { return false };
            // The drop target is rebuilt by the move, so run it after the signal.
            glib::idle_add_local_once(move || {
                let _ = WidgetExt::activate_action(&flow, "cmd.move", Some(&(from, i as u32).to_variant()));
            });
            true
        });
        w.add_controller(drop);
        w
    }

    fn send(&self, i: usize) {
        let Some(b) = self.button(i) else { return };
        let chunks = match b.chunks() {
            Ok(c) => c,
            Err(e) => return self.toast(&format!("{}: {e}", b.label)),
        };
        let Some(t) = self.target.borrow().as_ref().and_then(|f| f()) else {
            return self.toast("No terminal to send to; open or focus a pane first");
        };
        send(&t, chunks, b.hex, b.delay_ms);
    }

    fn shift(&self, from: usize, to: usize) {
        self.edit(|p| {
            if from < p.buttons.len() && to < p.buttons.len() && from != to {
                let b = p.buttons.remove(from);
                p.buttons.insert(to, b);
            }
        });
    }

    fn add_page(self: &Rc<Self>) {
        let p = self.clone();
        ask_name(&self.widget, "New Page", "", move |name| {
            let res = p.state.edit_commands(|pages| pages.push(Page { name, ..Default::default() }));
            p.page.set(p.state.commands.borrow().len().saturating_sub(1));
            p.refresh();
            if let Err(e) = res {
                p.toast(&e);
            }
        });
    }

    fn page_settings(self: &Rc<Self>) {
        let Some(cur) = self.current() else { return };
        let d = adw::AlertDialog::new(Some("Page Settings"), None);
        let name = gtk::Entry::new();
        name.set_text(&cur.name);
        name.set_placeholder_text(Some("Page name"));
        name.set_activates_default(true);
        let sessions = gtk::Entry::new();
        sessions.set_text(&cur.sessions.join(", "));
        sessions.set_placeholder_text(Some("U-Boot*, /dev/ttyUSB*, root@10.0.0.*"));
        sessions.set_activates_default(true);
        let hint = gtk::Label::new(Some(
            "Shown when a pane whose title or target matches gets the focus. Comma separated; * and ? match.",
        ));
        hint.add_css_class("dim-label");
        hint.add_css_class("caption");
        hint.set_wrap(true);
        hint.set_xalign(0.0);
        let label = gtk::Label::new(Some("Sessions"));
        label.set_xalign(0.0);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 6);
        body.append(&name);
        body.append(&label);
        body.append(&sessions);
        body.append(&hint);
        d.set_extra_child(Some(&body));
        d.add_responses(&[("cancel", "Cancel"), ("ok", "Save")]);
        d.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
        d.set_default_response(Some("ok"));
        d.set_close_response("cancel");
        let p = self.clone();
        d.connect_response(None, move |_, r| {
            let n = name.text().trim().to_string();
            if r != "ok" || n.is_empty() {
                return;
            }
            let globs: Vec<String> =
                sessions.text().split(',').map(|g| g.trim().to_string()).filter(|g| !g.is_empty()).collect();
            p.edit(|pg| {
                pg.name = n;
                pg.sessions = globs;
            });
        });
        d.present(Some(&self.widget));
    }

    fn delete_page(self: &Rc<Self>) {
        let Some(cur) = self.current() else { return };
        let at = self.page.get();
        let p = self.clone();
        let go = move || {
            let res = p.state.edit_commands(|pages| {
                if at < pages.len() {
                    pages.remove(at);
                }
                if pages.is_empty() {
                    pages.push(Page { name: "Commands".into(), ..Default::default() });
                }
            });
            if let Err(e) = res {
                p.toast(&e);
            }
        };
        if cur.buttons.is_empty() {
            return go();
        }
        let d = adw::AlertDialog::new(
            Some(&format!("Delete “{}”?", cur.name)),
            Some(&format!("Its {} button(s) are removed. Export first to keep a copy.", cur.buttons.len())),
        );
        d.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
        d.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        d.set_default_response(Some("cancel"));
        d.set_close_response("cancel");
        d.connect_response(None, move |_, r| {
            if r == "delete" {
                go();
            }
        });
        d.present(Some(&self.widget));
    }

    fn delete(&self, i: usize) {
        self.edit(|p| {
            if i < p.buttons.len() {
                p.buttons.remove(i);
            }
        });
    }

    fn duplicate(&self, i: usize) {
        let full = self.current().is_some_and(|p| p.buttons.len() >= MAX_BUTTONS);
        if full {
            return self.toast(&format!("A page holds up to {MAX_BUTTONS} buttons"));
        }
        self.edit(|p| {
            if let Some(b) = p.buttons.get(i).cloned() {
                p.buttons.insert(i + 1, b);
            }
        });
    }

    fn import(self: &Rc<Self>) {
        let p = self.clone();
        let dlg = gtk::FileDialog::new();
        dlg.set_default_filter(Some(&toml_filter()));
        dlg.open(self.window().as_ref(), None::<&gio::Cancellable>, move |res| {
            let Some(path) = res.ok().and_then(|f| f.path()) else { return };
            let fresh = match commands::import(&path) {
                Ok(f) => f,
                Err(e) => return p.toast(&format!("Import failed: {e}")),
            };
            let n = fresh.len();
            let first = p.state.commands.borrow().len();
            let res = p.state.edit_commands(|pages| pages.extend(fresh));
            if n > 0 {
                p.page.set(first);
                p.refresh();
            }
            match res {
                Ok(()) => p.toast(&format!("Imported {n} page(s)")),
                Err(e) => p.toast(&e),
            }
        });
    }

    fn export(self: &Rc<Self>) {
        let p = self.clone();
        let dlg = gtk::FileDialog::new();
        dlg.set_initial_name(Some("commands.toml"));
        dlg.set_default_filter(Some(&toml_filter()));
        dlg.save(self.window().as_ref(), None::<&gio::Cancellable>, move |res| {
            let Some(path) = res.ok().and_then(|f| f.path()) else { return };
            match commands::export(&p.state.commands.borrow(), &path) {
                Ok(()) => p.toast(&format!("Exported to {}", path.display())),
                Err(e) => p.toast(&format!("Export failed: {e}")),
            }
        });
    }
}

/// Sends the chunks to `t`, `delay_ms` apart. Stream panes get the bytes
/// on their link; other terminals get them as typed input.
fn send(t: &vte::Terminal, chunks: Vec<Vec<u8>>, hex: bool, delay_ms: u32) {
    let mut wait = 0u64;
    for c in chunks {
        let w = t.downgrade();
        let go = move || {
            if let Some(t) = w.upgrade() {
                if !streampane::write(&t, &c, hex) {
                    t.feed_child(&c);
                }
            }
        };
        if wait == 0 {
            go();
        } else {
            glib::timeout_add_local_once(Duration::from_millis(wait), go);
        }
        wait += u64::from(delay_ms);
    }
}

/// "Ctrl+1" for "<Control>1".
fn key_label(key: &str) -> String {
    gtk::accelerator_parse(key).map(|(k, m)| gtk::accelerator_get_label(k, m).to_string()).unwrap_or_else(|| key.to_string())
}

fn preview(b: &Button) -> String {
    const MAX: usize = 200;
    let mut s: String = b.text.chars().take(MAX).collect();
    if b.text.chars().count() > MAX {
        s.push('…');
    }
    let kind = if b.hex { "hex".to_string() } else { format!("text, {}", b.eol.label()) };
    format!("{s}\n({kind})")
}

fn context_menu(i: usize) -> gio::Menu {
    let item = |label: &str, action: &str| {
        let m = gio::MenuItem::new(Some(label), None);
        m.set_action_and_target_value(Some(action), Some(&(i as u32).to_variant()));
        m
    };
    let main = gio::Menu::new();
    main.append_item(&item("Send", "cmd.send"));
    main.append_item(&item("Edit…", "cmd.edit"));
    main.append_item(&item("Duplicate", "cmd.duplicate"));
    let moves = gio::Menu::new();
    moves.append_item(&item("Move Left", "cmd.left"));
    moves.append_item(&item("Move Right", "cmd.right"));
    let del = gio::Menu::new();
    del.append_item(&item("Delete", "cmd.delete"));
    let m = gio::Menu::new();
    m.append_section(None, &main);
    m.append_section(None, &moves);
    m.append_section(None, &del);
    m
}

fn actions(p: &Rc<Panel>) -> gio::SimpleActionGroup {
    let group = gio::SimpleActionGroup::new();
    let index = |name: &str, f: fn(&Rc<Panel>, usize)| {
        let a = gio::SimpleAction::new(name, Some(glib::VariantTy::UINT32));
        let w = Rc::downgrade(p);
        a.connect_activate(move |_, v| {
            if let (Some(p), Some(i)) = (w.upgrade(), v.and_then(|v| v.get::<u32>())) {
                f(&p, i as usize);
            }
        });
        group.add_action(&a);
    };
    index("send", |p, i| p.send(i));
    index("edit", |p, i| {
        if let Some(b) = p.button(i) {
            editor(p, Some(i), b);
        }
    });
    index("duplicate", |p, i| p.duplicate(i));
    index("left", |p, i| {
        if i > 0 {
            p.shift(i, i - 1);
        }
    });
    index("right", |p, i| p.shift(i, i + 1));
    index("delete", |p, i| p.delete(i));

    let plain = |name: &str, f: fn(&Rc<Panel>)| {
        let a = gio::SimpleAction::new(name, None);
        let w = Rc::downgrade(p);
        a.connect_activate(move |_, _| {
            if let Some(p) = w.upgrade() {
                f(&p);
            }
        });
        group.add_action(&a);
    };
    plain("add", |p| {
        if p.current().is_some_and(|pg| pg.buttons.len() >= MAX_BUTTONS) {
            return p.toast(&format!("A page holds up to {MAX_BUTTONS} buttons"));
        }
        editor(p, None, Button::default());
    });
    plain("page-new", |p| p.add_page());
    plain("page-settings", |p| p.page_settings());
    plain("page-delete", |p| p.delete_page());
    plain("import", |p| p.import());
    plain("export", |p| p.export());

    let a = gio::SimpleAction::new_stateful("fkeys", None, &p.state.cfg.borrow().command_keys.to_variant());
    let w = Rc::downgrade(p);
    a.connect_activate(move |a, _| {
        let on = !a.state().and_then(|v| v.get::<bool>()).unwrap_or(false);
        a.set_state(&on.to_variant());
        if let Some(p) = w.upgrade() {
            p.state.edit_cfg(|c| c.command_keys = on);
            p.state.commands_changed();
        }
    });
    group.add_action(&a);

    let a = gio::SimpleAction::new("move", Some(&glib::VariantType::new("(uu)").expect("type")));
    let w: Weak<Panel> = Rc::downgrade(p);
    a.connect_activate(move |_, v| {
        if let (Some(p), Some((from, to))) = (w.upgrade(), v.and_then(|v| v.get::<(u32, u32)>())) {
            p.shift(from as usize, to as usize);
        }
    });
    group.add_action(&a);
    group
}

fn toml_filter() -> gtk::FileFilter {
    let f = gtk::FileFilter::new();
    f.set_name(Some("Command pages (*.toml)"));
    f.add_pattern("*.toml");
    f
}

/// Asks for a name; `done` gets the trimmed, non-empty result.
fn ask_name(parent: &gtk::Box, title: &str, init: &str, done: impl Fn(String) + 'static) {
    let d = adw::AlertDialog::new(Some(title), None);
    let entry = gtk::Entry::new();
    entry.set_text(init);
    entry.set_placeholder_text(Some("Page name"));
    entry.set_activates_default(true);
    d.set_extra_child(Some(&entry));
    d.add_responses(&[("cancel", "Cancel"), ("ok", "OK")]);
    d.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
    d.set_default_response(Some("ok"));
    d.set_close_response("cancel");
    d.connect_response(None, move |_, r| {
        let name = entry.text().trim().to_string();
        if r == "ok" && !name.is_empty() {
            done(name);
        }
    });
    d.present(Some(parent));
}

fn combo(title: &str, items: &[&str], at: usize) -> adw::ComboRow {
    let r = adw::ComboRow::new();
    r.set_title(title);
    r.set_model(Some(&gtk::StringList::new(items)));
    r.set_selected(at as u32);
    r
}

/// Button editor. `at` is the index to replace; None appends.
fn editor(p: &Rc<Panel>, at: Option<usize>, init: Button) {
    let label = adw::EntryRow::new();
    label.set_title("Label");
    label.set_text(&init.label);

    let text = gtk::TextView::new();
    text.set_monospace(true);
    text.set_wrap_mode(gtk::WrapMode::WordChar);
    for m in [4, 8] {
        text.set_top_margin(m);
        text.set_bottom_margin(m);
    }
    text.set_left_margin(8);
    text.set_right_margin(8);
    text.buffer().set_text(&init.text);
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_min_content_height(140);
    scroll.set_child(Some(&text));
    let frame = gtk::Frame::new(None);
    frame.set_child(Some(&scroll));

    let mode = combo("Payload Type", &["Text", "Hex bytes"], usize::from(init.hex));
    let ends: Vec<&str> = Ending::ALL.iter().map(|e| e.label()).collect();
    let eol = combo("Line Ending", &ends, Ending::ALL.iter().position(|e| *e == init.eol).unwrap_or(0));
    let delay = adw::SpinRow::with_range(0.0, 60_000.0, 50.0);
    delay.set_title("Delay Between Lines");
    delay.set_subtitle("Milliseconds");
    delay.set_value(f64::from(init.delay_ms));
    let names: Vec<&str> = COLORS.iter().map(|c| c.0).collect();
    let color = combo("Color", &names, COLORS.iter().position(|c| c.1 == init.color.as_deref()).unwrap_or(0));

    let key: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(init.key.clone()));
    let shortcut = adw::ActionRow::new();
    shortcut.set_title("Shortcut");
    shortcut.set_subtitle("Works while a terminal has the focus");
    let accel = gtk::ShortcutLabel::new(init.key.as_deref().unwrap_or(""));
    accel.set_disabled_text("None");
    accel.set_valign(gtk::Align::Center);
    let record = gtk::ToggleButton::with_label("Set…");
    record.set_valign(gtk::Align::Center);
    let clear = gtk::Button::from_icon_name("edit-clear-symbolic");
    clear.add_css_class("flat");
    clear.set_valign(gtk::Align::Center);
    clear.set_tooltip_text(Some("No shortcut"));
    shortcut.add_suffix(&accel);
    shortcut.add_suffix(&record);
    shortcut.add_suffix(&clear);
    record.connect_toggled(|b| b.set_label(if b.is_active() { "Press keys…" } else { "Set…" }));
    let (k, a) = (key.clone(), accel.clone());
    clear.connect_clicked(move |_| {
        k.replace(None);
        a.set_accelerator("");
    });

    let top = adw::PreferencesGroup::new();
    top.add(&label);
    let body = adw::PreferencesGroup::new();
    body.set_title("Payload");
    body.add(&frame);
    let opts = adw::PreferencesGroup::new();
    opts.add(&mode);
    opts.add(&eol);
    opts.add(&delay);
    opts.add(&color);
    opts.add(&shortcut);

    let sync = {
        let (mode, eol, delay, body) = (mode.clone(), eol.clone(), delay.clone(), body.clone());
        move || {
            let hex = mode.selected() == 1;
            body.set_description(Some(if hex { HEX_HINT } else { TEXT_HINT }));
            eol.set_sensitive(!hex);
            delay.set_sensitive(!hex);
        }
    };
    sync();
    mode.connect_selected_notify(move |_| sync());

    let page = adw::PreferencesPage::new();
    page.add(&top);
    page.add(&body);
    page.add(&opts);
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
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&toasts));

    let dlg = adw::Dialog::new();
    dlg.set_title(if at.is_some() { "Edit Button" } else { "New Button" });
    dlg.set_content_width(460);
    dlg.set_content_height(600);
    dlg.set_child(Some(&view));

    // While "Set…" is on, the next key combination becomes the shortcut.
    let catch = gtk::EventControllerKey::new();
    catch.set_propagation_phase(gtk::PropagationPhase::Capture);
    let (k, a, r, t) = (key.clone(), accel.clone(), record.clone(), toasts.clone());
    catch.connect_key_pressed(move |_, keyval, _, state| {
        if !r.is_active() {
            return glib::Propagation::Proceed;
        }
        let mods = state & gtk::accelerator_get_default_mod_mask();
        if keyval == gdk::Key::Escape && mods.is_empty() {
            r.set_active(false);
            return glib::Propagation::Stop;
        }
        // Modifier alone: wait for the rest.
        if !gtk::accelerator_valid(keyval, mods) {
            return glib::Propagation::Stop;
        }
        let v = keyval.into_glib();
        let fkey = (gdk::Key::F1.into_glib()..=gdk::Key::F35.into_glib()).contains(&v);
        let held = gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK | gdk::ModifierType::SUPER_MASK;
        if !fkey && !mods.intersects(held) {
            t.add_toast(adw::Toast::new("Use Ctrl, Alt or Super with this key, or a function key"));
            return glib::Propagation::Stop;
        }
        let name = gtk::accelerator_name(keyval, mods).to_string();
        a.set_accelerator(&name);
        k.replace(Some(name));
        r.set_active(false);
        glib::Propagation::Stop
    });
    dlg.add_controller(catch);

    let d = dlg.clone();
    cancel.connect_clicked(move |_| {
        d.close();
    });

    let parent = p.widget.clone();
    let name = label.clone();
    let (d, p) = (dlg.clone(), p.clone());
    let submit = Rc::new(move || {
        let buf = text.buffer();
        let body = buf.text(&buf.start_iter(), &buf.end_iter(), false).to_string();
        let mut b = Button {
            label: label.text().trim().to_string(),
            text: body.trim_end_matches(['\n', '\r']).to_string(),
            hex: mode.selected() == 1,
            eol: Ending::ALL.get(eol.selected() as usize).copied().unwrap_or_default(),
            delay_ms: delay.value() as u32,
            color: COLORS.get(color.selected() as usize).and_then(|c| c.1).map(str::to_string),
            key: key.borrow().clone(),
        };
        if b.text.trim().is_empty() && b.eol == Ending::None {
            return toasts.add_toast(adw::Toast::new("Nothing to send"));
        }
        if let Err(e) = b.chunks() {
            return toasts.add_toast(adw::Toast::new(&e));
        }
        if b.label.is_empty() {
            b.label = b.text.lines().next().unwrap_or("").chars().take(24).collect();
        }
        let taken = b.key.as_ref().and_then(|k| {
            p.current()?.buttons.iter().enumerate().find(|(i, o)| Some(*i) != at && o.key.as_ref() == Some(k)).map(|(_, o)| o.label.clone())
        });
        if let Some(other) = taken {
            return toasts.add_toast(adw::Toast::new(&format!("Shortcut already used by “{other}”")));
        }
        let full = at.is_none() && p.current().is_some_and(|pg| pg.buttons.len() >= MAX_BUTTONS);
        if full {
            return toasts.add_toast(adw::Toast::new(&format!("A page holds up to {MAX_BUTTONS} buttons")));
        }
        p.edit(|pg| match at {
            Some(i) if i < pg.buttons.len() => pg.buttons[i] = b,
            _ => pg.buttons.push(b),
        });
        d.close();
    });
    let s = submit.clone();
    save.connect_clicked(move |_| s());
    name.connect_entry_activated(move |_| submit());

    dlg.present(Some(&parent));
}
