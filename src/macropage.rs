// Macros page of the side panel: list, run, record and edit macros.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::keys;
use crate::macros::{self, Macro, Step};
use crate::multi;
use crate::state::State;

type Target = Box<dyn Fn() -> Option<crate::vt::View>>;

pub struct Page {
    pub widget: gtk::Box,
    state: Rc<State>,
    list: gtk::ListBox,
    empty: gtk::Label,
    record: gtk::ToggleButton,
    stop: gtk::Button,
    macros: RefCell<Vec<Macro>>,
    target: RefCell<Option<Target>>,
    /// Stop flags of running macros.
    running: RefCell<Vec<Rc<Cell<bool>>>>,
    keys: gtk::ShortcutController,
}

pub fn build(state: &Rc<State>) -> Rc<Page> {
    let record = gtk::ToggleButton::with_label("● Record");
    record.set_tooltip_text(Some("Record what you type in the focused pane"));
    record.set_focus_on_click(false);
    let stop = gtk::Button::with_label("■ Stop");
    stop.set_tooltip_text(Some("Stop running macros"));
    stop.set_visible(false);
    let add = gtk::Button::from_icon_name("list-add-symbolic");
    add.add_css_class("flat");
    add.set_tooltip_text(Some("New macro"));
    let head = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    head.set_margin_start(8);
    head.set_margin_end(8);
    head.set_margin_top(4);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    head.append(&record);
    head.append(&stop);
    head.append(&spacer);
    head.append(&add);

    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    let empty = gtk::Label::new(Some("No macros yet. Record one or add one with +."));
    empty.add_css_class("dim-label");
    empty.set_wrap(true);
    empty.set_margin_top(12);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
    body.set_margin_start(8);
    body.set_margin_end(8);
    body.set_margin_top(6);
    body.append(&list);
    body.append(&empty);
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
    scroll.set_vexpand(true);
    scroll.set_child(Some(&body));

    let widget = gtk::Box::new(gtk::Orientation::Vertical, 4);
    widget.append(&head);
    widget.append(&scroll);

    let keys = gtk::ShortcutController::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);

    let p = Rc::new(Page {
        widget,
        state: state.clone(),
        list,
        empty,
        record,
        stop,
        macros: RefCell::new(macros::load()),
        target: RefCell::new(None),
        running: RefCell::new(Vec::new()),
        keys,
    });
    let keep = RefCell::new(Some(p.clone()));
    p.widget.connect_destroy(move |_| drop(keep.take()));

    let w = Rc::downgrade(&p);
    add.connect_clicked(move |_| {
        if let Some(p) = w.upgrade() {
            editor(&p, None, Macro { name: "New macro".into(), ..Default::default() });
        }
    });
    let w = Rc::downgrade(&p);
    p.record.connect_toggled(move |b| {
        if let Some(p) = w.upgrade() {
            p.recording(b.is_active());
        }
    });
    let w = Rc::downgrade(&p);
    p.stop.connect_clicked(move |_| {
        if let Some(p) = w.upgrade() {
            for s in p.running.borrow().iter() {
                s.set(true);
            }
        }
    });
    // Another window may have changed the macros.
    let w = Rc::downgrade(&p);
    p.widget.connect_map(move |_| {
        if let Some(p) = w.upgrade() {
            p.macros.replace(macros::load());
            p.fill();
        }
    });
    p.fill();
    p
}

impl Page {
    /// Sets how the pane a macro runs on is found (normally the focused one).
    pub fn set_target(&self, f: impl Fn() -> Option<crate::vt::View> + 'static) {
        self.target.replace(Some(Box::new(f)));
    }

    /// Macro shortcuts, for the window to handle.
    pub fn keys(&self) -> gtk::ShortcutController {
        self.keys.clone()
    }

    fn toast(&self, msg: &str) {
        let overlay = self
            .widget
            .ancestor(adw::ToastOverlay::static_type())
            .and_downcast::<adw::ToastOverlay>()
            .or_else(|| self.widget.root().and_then(|r| find_toasts(r.upcast_ref())));
        match overlay {
            Some(t) => t.add_toast(adw::Toast::new(msg)),
            None => crate::warn(msg),
        }
    }

    fn save(self: &Rc<Self>, list: Vec<Macro>) {
        if let Err(e) = macros::save(&list) {
            self.toast(&format!("Save macros: {e}"));
        }
        self.macros.replace(list);
        self.fill();
    }

    fn fill(self: &Rc<Self>) {
        self.list.remove_all();
        let list = self.macros.borrow().clone();
        for (i, m) in list.iter().enumerate() {
            self.list.append(&self.row(i, m));
        }
        self.list.set_visible(!list.is_empty());
        self.empty.set_visible(list.is_empty());
        self.bind();
    }

    /// Rebuilds the macro shortcuts.
    fn bind(self: &Rc<Self>) {
        let old: Vec<gtk::Shortcut> = (0..self.keys.n_items()).filter_map(|i| self.keys.item(i).and_downcast()).collect();
        for s in old {
            self.keys.remove_shortcut(&s);
        }
        for (i, m) in self.macros.borrow().iter().enumerate() {
            let Some(trigger) = m.key.as_deref().and_then(gtk::ShortcutTrigger::parse_string) else { continue };
            let w = Rc::downgrade(self);
            let action = keys::in_terminal(move |t| {
                if let Some(p) = w.upgrade() {
                    p.run_on(i, vec![t.clone()]);
                }
            });
            self.keys.add_shortcut(gtk::Shortcut::new(Some(trigger), Some(action)));
        }
    }

    fn row(self: &Rc<Self>, i: usize, m: &Macro) -> adw::ActionRow {
        let r = adw::ActionRow::new();
        r.set_title(&glib::markup_escape_text(&m.name));
        let mut note = vec![format!("{} step{}", m.steps.len(), if m.steps.len() == 1 { "" } else { "s" })];
        if let Some(k) = &m.key {
            note.push(keys::label(k));
        }
        if !m.sessions.is_empty() {
            note.push(m.sessions.join(", "));
        }
        r.set_subtitle(&glib::markup_escape_text(&note.join(" · ")));
        let run = gtk::Button::from_icon_name("media-playback-start-symbolic");
        run.set_tooltip_text(Some("Run on the focused pane (all selected panes in multi-exec)"));
        let edit = gtk::Button::from_icon_name("document-edit-symbolic");
        edit.set_tooltip_text(Some("Edit"));
        let del = gtk::Button::from_icon_name("user-trash-symbolic");
        del.set_tooltip_text(Some("Delete"));
        for b in [&run, &edit, &del] {
            b.add_css_class("flat");
            b.set_valign(gtk::Align::Center);
            b.set_focus_on_click(false);
            r.add_suffix(b);
        }
        let w = Rc::downgrade(self);
        run.connect_clicked(move |_| {
            if let Some(p) = w.upgrade() {
                p.run(i);
            }
        });
        let w = Rc::downgrade(self);
        let m2 = m.clone();
        edit.connect_clicked(move |_| {
            if let Some(p) = w.upgrade() {
                editor(&p, Some(i), m2.clone());
            }
        });
        let w = Rc::downgrade(self);
        del.connect_clicked(move |_| {
            if let Some(p) = w.upgrade() {
                let mut list = p.macros.borrow().clone();
                if i < list.len() {
                    list.remove(i);
                }
                p.save(list);
            }
        });
        r
    }

    /// Runs macro `i` on the multi-exec panes, else on the focused pane.
    fn run(self: &Rc<Self>, i: usize) {
        let picked = if multi::on() { multi::targets() } else { Vec::new() };
        let panes = if picked.is_empty() { self.target.borrow().as_ref().and_then(|f| f()).into_iter().collect() } else { picked };
        if panes.is_empty() {
            return self.toast("No pane to run on; open or focus a pane first");
        }
        self.run_on(i, panes);
    }

    fn run_on(self: &Rc<Self>, i: usize, panes: Vec<crate::vt::View>) {
        let Some(m) = self.macros.borrow().get(i).cloned() else { return };
        for t in panes {
            if let Some(l) = self.state.launch(&t) {
                if !m.matches(&l.title, &l.target) {
                    self.toast(&format!("“{}” is not for {}", m.name, l.title));
                    continue;
                }
            }
            let stop = Rc::new(Cell::new(false));
            self.running.borrow_mut().push(stop.clone());
            self.stop.set_visible(true);
            let (w, m) = (Rc::downgrade(self), m.clone());
            glib::spawn_future_local(async move {
                let res = macros::run(&t, &m, stop.clone()).await;
                let Some(p) = w.upgrade() else { return };
                p.running.borrow_mut().retain(|s| !Rc::ptr_eq(s, &stop));
                p.stop.set_visible(!p.running.borrow().is_empty());
                match res {
                    Ok(()) => p.toast(&format!("“{}” done", m.name)),
                    Err(e) => p.toast(&format!("“{}”: {e}", m.name)),
                }
            });
        }
    }

    fn recording(self: &Rc<Self>, on: bool) {
        if on {
            let Some(t) = self.target.borrow().as_ref().and_then(|f| f()) else {
                self.toast("Focus the pane to record first");
                self.record.set_active(false);
                return;
            };
            macros::record(&t);
            self.record.set_label("■ Stop recording");
            self.record.add_css_class("destructive-action");
            return;
        }
        self.record.set_label("● Record");
        self.record.remove_css_class("destructive-action");
        if macros::recording().is_none() {
            return;
        }
        let steps = macros::stop_recording();
        if steps.is_empty() {
            return self.toast("Nothing was typed");
        }
        editor(self, None, Macro { name: "Recorded macro".into(), steps, ..Default::default() });
    }
}

fn find_toasts(w: &gtk::Widget) -> Option<adw::ToastOverlay> {
    if let Some(t) = w.downcast_ref::<adw::ToastOverlay>() {
        return Some(t.clone());
    }
    let mut c = w.first_child();
    while let Some(x) = c {
        if let Some(t) = find_toasts(&x) {
            return Some(t);
        }
        c = x.next_sibling();
    }
    None
}

const KINDS: [&str; 3] = ["Send", "Wait", "Expect"];

/// One editable step.
struct StepRow {
    root: gtk::Box,
    kind: gtk::DropDown,
    value: gtk::Entry,
    secs: gtk::SpinButton,
}

impl StepRow {
    fn new(step: &Step) -> Self {
        let kind = gtk::DropDown::from_strings(&KINDS);
        let value = gtk::Entry::new();
        value.set_hexpand(true);
        value.add_css_class("monospace");
        let secs = gtk::SpinButton::with_range(1.0, 3600.0, 1.0);
        secs.set_tooltip_text(Some("Timeout in seconds"));
        let (k, v, t) = match step {
            Step::Send { text } => (0, text.clone(), 10),
            Step::Wait { ms } => (1, ms.to_string(), 10),
            Step::Expect { pattern, timeout_ms } => (2, pattern.clone(), timeout_ms / 1000),
        };
        kind.set_selected(k);
        value.set_text(&v);
        secs.set_value(f64::from(t.max(1)));
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        root.append(&kind);
        root.append(&value);
        root.append(&secs);
        let sync = {
            let (value, secs) = (value.clone(), secs.clone());
            move |k: u32| {
                secs.set_visible(k == 2);
                value.set_placeholder_text(Some(match k {
                    0 => "Text; \\r is Enter, \\x03 is Ctrl+C",
                    1 => "Milliseconds",
                    _ => "Regex to wait for, e.g. \\$ $ or login:",
                }));
            }
        };
        sync(k);
        kind.connect_selected_notify(move |d| sync(d.selected()));
        Self { root, kind, value, secs }
    }

    fn step(&self) -> Result<Step, String> {
        let v = self.value.text().to_string();
        Ok(match self.kind.selected() {
            0 => Step::Send { text: v },
            1 => Step::Wait { ms: v.trim().parse().map_err(|_| format!("wait: '{v}' is not a number of milliseconds"))? },
            _ => Step::Expect { pattern: v, timeout_ms: (self.secs.value() as u32).max(1) * 1000 },
        })
    }
}

type Rows = Rc<RefCell<Vec<Rc<StepRow>>>>;

/// Lays out the step rows, each with move up and delete buttons.
fn redraw(list: &gtk::ListBox, rows: &Rows) {
    list.remove_all();
    for (i, r) in rows.borrow().iter().enumerate() {
        if let Some(parent) = r.root.parent() {
            if let Some(b) = parent.downcast_ref::<gtk::Box>() {
                b.remove(&r.root);
            }
        }
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        let num = gtk::Label::new(Some(&format!("{}", i + 1)));
        num.add_css_class("dim-label");
        num.set_width_chars(2);
        let up = gtk::Button::from_icon_name("go-up-symbolic");
        up.add_css_class("flat");
        up.set_sensitive(i > 0);
        let del = gtk::Button::from_icon_name("user-trash-symbolic");
        del.add_css_class("flat");
        line.append(&num);
        line.append(&r.root);
        line.append(&up);
        line.append(&del);
        let (l, rs) = (list.clone(), rows.clone());
        up.connect_clicked(move |_| {
            rs.borrow_mut().swap(i - 1, i);
            redraw(&l, &rs);
        });
        let (l, rs) = (list.clone(), rows.clone());
        del.connect_clicked(move |_| {
            rs.borrow_mut().remove(i);
            redraw(&l, &rs);
        });
        list.append(&line);
    }
}

/// Macro editor. `at` is the index to replace; None appends.
fn editor(p: &Rc<Page>, at: Option<usize>, init: Macro) {
    let name = adw::EntryRow::new();
    name.set_title("Name");
    name.set_text(&init.name);
    let sessions = adw::EntryRow::new();
    sessions.set_title("Sessions (empty: all panes)");
    sessions.set_text(&init.sessions.join(", "));
    sessions.set_tooltip_text(Some("Comma separated patterns on pane title or target, e.g. /dev/ttyUSB*, root@10.0.0.*"));

    let steps = gtk::ListBox::new();
    steps.set_selection_mode(gtk::SelectionMode::None);
    steps.add_css_class("boxed-list");
    let rows: Rows = Rc::new(RefCell::new(init.steps.iter().map(|s| Rc::new(StepRow::new(s))).collect()));
    redraw(&steps, &rows);
    let adds = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    for (label, step) in [
        ("+ Send", Step::Send { text: String::new() }),
        ("+ Wait", Step::Wait { ms: 500 }),
        ("+ Expect", Step::Expect { pattern: String::new(), timeout_ms: 10_000 }),
    ] {
        let b = gtk::Button::with_label(label);
        let (l, rs) = (steps.clone(), rows.clone());
        b.connect_clicked(move |_| {
            rs.borrow_mut().push(Rc::new(StepRow::new(&step)));
            redraw(&l, &rs);
        });
        adds.append(&b);
    }

    let general = adw::PreferencesGroup::new();
    general.add(&name);
    general.add(&sessions);
    let body = adw::PreferencesGroup::new();
    body.set_title("Steps");
    body.set_description(Some("Send types text (\\r is Enter). Expect waits for a regex in the output after the last Send."));
    body.add(&steps);
    body.add(&adds);
    let page = adw::PreferencesPage::new();
    page.add(&general);
    page.add(&body);
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
    dlg.set_title(if at.is_some() { "Edit Macro" } else { "New Macro" });
    dlg.set_content_width(620);
    dlg.set_content_height(600);
    dlg.set_child(Some(&view));

    let (shortcut, key) = keys::row(init.key.as_deref(), &dlg, &toasts);
    general.add(&shortcut);

    let d = dlg.clone();
    cancel.connect_clicked(move |_| {
        d.close();
    });
    let parent = p.widget.clone();
    let (d, p) = (dlg.clone(), p.clone());
    save.connect_clicked(move |_| {
        let built: Result<Vec<Step>, String> = rows.borrow().iter().map(|r| r.step()).collect();
        let m = match built {
            Ok(steps) => Macro {
                name: name.text().trim().to_string(),
                key: key.borrow().clone(),
                sessions: sessions.text().split(',').map(|g| g.trim().to_string()).filter(|g| !g.is_empty()).collect(),
                steps,
            },
            Err(e) => return toasts.add_toast(adw::Toast::new(&e)),
        };
        if m.name.is_empty() {
            return toasts.add_toast(adw::Toast::new("Give the macro a name"));
        }
        if let Err(e) = m.check() {
            return toasts.add_toast(adw::Toast::new(&e));
        }
        let mut list = p.macros.borrow().clone();
        match at {
            Some(i) if i < list.len() => list[i] = m,
            _ => list.push(m),
        }
        p.save(list);
        d.close();
    });
    dlg.present(Some(&parent));
}
