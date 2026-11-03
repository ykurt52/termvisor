// Log page of the bottom dock: timestamps, highlight and log file of the
// focused pane, and the highlight rules shared by all panes.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::output::{self, LogMode, Rule, COLORS};
use crate::state::State;

type Target = Box<dyn Fn() -> Option<crate::vt::View>>;

pub struct Panel {
    pub widget: gtk::Box,
    state: Rc<State>,
    pane: gtk::Label,
    stamps: gtk::ToggleButton,
    marks: gtk::ToggleButton,
    mode: gtk::DropDown,
    file: gtk::Label,
    folder: gtk::Button,
    list: gtk::ListBox,
    rules: RefCell<Vec<Rule>>,
    target: RefCell<Option<Target>>,
    shown: RefCell<glib::WeakRef<crate::vt::View>>,
    /// Set while controls are filled from a pane, so their signals are ignored.
    syncing: Cell<bool>,
}

fn toggle(label: &str, tip: &str) -> gtk::ToggleButton {
    let b = gtk::ToggleButton::with_label(label);
    b.set_tooltip_text(Some(tip));
    b.set_focus_on_click(false);
    b
}

pub fn build(state: &Rc<State>) -> Rc<Panel> {
    let pane = gtk::Label::new(None);
    pane.add_css_class("heading");
    pane.set_xalign(0.0);
    pane.set_ellipsize(gtk::pango::EllipsizeMode::End);

    let stamps = toggle("Timestamps", "Time of arrival before each line");
    let marks = toggle("Highlight", "Color matches of the highlight rules");
    let modes: Vec<&str> = LogMode::ALL.iter().map(|m| m.label()).collect();
    let mode = gtk::DropDown::from_strings(&modes);
    mode.set_tooltip_text(Some("Log this pane to a file"));
    let folder = gtk::Button::from_icon_name("folder-open-symbolic");
    folder.add_css_class("flat");
    folder.set_tooltip_text(Some("Open the log folder"));
    let file = gtk::Label::new(None);
    file.add_css_class("dim-label");
    file.add_css_class("caption");
    file.set_xalign(0.0);
    file.set_selectable(true);
    file.set_ellipsize(gtk::pango::EllipsizeMode::Start);

    let shows = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    shows.append(&stamps);
    shows.append(&marks);
    let logs = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let log_label = gtk::Label::new(Some("Log to file"));
    logs.append(&log_label);
    logs.append(&mode);
    logs.append(&folder);

    let left = gtk::Box::new(gtk::Orientation::Vertical, 6);
    left.set_size_request(300, -1);
    left.set_margin_start(8);
    left.set_margin_end(8);
    left.set_margin_top(6);
    left.append(&pane);
    left.append(&shows);
    left.append(&logs);
    left.append(&file);

    let title = gtk::Label::new(Some("Highlight Rules"));
    title.add_css_class("heading");
    title.set_xalign(0.0);
    title.set_hexpand(true);
    let add = gtk::Button::from_icon_name("list-add-symbolic");
    add.add_css_class("flat");
    add.set_tooltip_text(Some("Add a rule"));
    let reset = gtk::Button::from_icon_name("edit-undo-symbolic");
    reset.add_css_class("flat");
    reset.set_tooltip_text(Some("Restore the default rules"));
    let head = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    head.append(&title);
    head.append(&reset);
    head.append(&add);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("rules");
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
    scroll.set_vexpand(true);
    scroll.set_child(Some(&list));

    let right = gtk::Box::new(gtk::Orientation::Vertical, 4);
    right.set_hexpand(true);
    right.set_margin_start(8);
    right.set_margin_end(8);
    right.set_margin_top(4);
    right.append(&head);
    right.append(&scroll);

    let widget = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    widget.add_css_class("logpanel");
    widget.append(&left);
    widget.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    widget.append(&right);

    let p = Rc::new(Panel {
        widget,
        state: state.clone(),
        pane,
        stamps,
        marks,
        mode,
        file,
        folder,
        list,
        rules: RefCell::new(output::load_rules()),
        target: RefCell::new(None),
        shown: RefCell::new(glib::WeakRef::new()),
        syncing: Cell::new(false),
    });
    let keep = RefCell::new(Some(p.clone()));
    p.widget.connect_destroy(move |_| drop(keep.take()));

    let w = Rc::downgrade(&p);
    p.stamps.connect_toggled(move |b| {
        if let Some(p) = w.upgrade() {
            p.change(|o| o.filter.stamps = b.is_active());
        }
    });
    let w = Rc::downgrade(&p);
    p.marks.connect_toggled(move |b| {
        if let Some(p) = w.upgrade() {
            p.change(|o| o.filter.marks = b.is_active());
        }
    });
    let w = Rc::downgrade(&p);
    p.mode.connect_selected_notify(move |d| {
        let Some(p) = w.upgrade() else { return };
        let m = LogMode::ALL.get(d.selected() as usize).copied().unwrap_or_default();
        let mut err = None;
        p.change(|o| err = o.set_mode(m).err());
        if let Some(e) = err {
            p.toast(&format!("Log: {e}"));
        }
        p.sync();
    });
    let w = Rc::downgrade(&p);
    p.folder.connect_clicked(move |_| {
        let Some(p) = w.upgrade() else { return };
        let dir = output::log_dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return p.toast(&format!("{}: {e}", dir.display()));
        }
        let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(&dir)));
        launcher.launch(p.window().as_ref(), None::<&gio::Cancellable>, |_| {});
    });
    let w = Rc::downgrade(&p);
    add.connect_clicked(move |_| {
        if let Some(p) = w.upgrade() {
            editor(&p, None, Rule::default());
        }
    });
    let w = Rc::downgrade(&p);
    reset.connect_clicked(move |_| {
        if let Some(p) = w.upgrade() {
            p.save(output::default_rules());
        }
    });
    // Another window may have changed the rules.
    let w = Rc::downgrade(&p);
    p.widget.connect_map(move |_| {
        if let Some(p) = w.upgrade() {
            p.rules.replace(output::load_rules());
            p.fill();
            p.sync();
        }
    });
    p.fill();
    p.sync();
    p
}

impl Panel {
    /// Sets how the shown pane is found (normally the focused one).
    pub fn set_target(&self, f: impl Fn() -> Option<crate::vt::View> + 'static) {
        self.target.replace(Some(Box::new(f)));
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

    fn out(&self) -> Option<Rc<RefCell<output::Out>>> {
        self.shown.borrow().upgrade().and_then(|t| output::of(&t))
    }

    fn change(&self, f: impl FnOnce(&mut output::Out)) {
        if self.syncing.get() {
            return;
        }
        if let Some(o) = self.out() {
            f(&mut o.borrow_mut());
        }
    }

    /// Stacks the pane controls over the rules (for a narrow dock).
    pub fn set_vertical(&self, on: bool) {
        let (outer, line) = if on {
            (gtk::Orientation::Vertical, gtk::Orientation::Horizontal)
        } else {
            (gtk::Orientation::Horizontal, gtk::Orientation::Vertical)
        };
        self.widget.set_orientation(outer);
        let mut child = self.widget.first_child();
        while let Some(c) = child {
            if let Some(sep) = c.downcast_ref::<gtk::Separator>() {
                sep.set_orientation(line);
            }
            child = c.next_sibling();
        }
    }

    /// Shows the settings of the focused pane.
    pub fn sync(&self) {
        let t = self.target.borrow().as_ref().and_then(|f| f());
        self.shown.replace(t.as_ref().map(|t| t.downgrade()).unwrap_or_default());
        let out = t.as_ref().and_then(output::of);
        let title = t.as_ref().and_then(|t| self.state.launch(t)).map(|l| l.title);
        self.syncing.set(true);
        match (&out, title) {
            (Some(o), title) => {
                let o = o.borrow();
                self.pane.set_text(&title.unwrap_or_else(|| o.title.clone()));
                self.stamps.set_active(o.filter.stamps);
                self.marks.set_active(o.filter.marks);
                self.mode.set_selected(LogMode::ALL.iter().position(|m| *m == o.mode()).unwrap_or(0) as u32);
                let file = o.log.as_ref().map(|l| l.path.display().to_string());
                self.file.set_text(file.as_deref().unwrap_or("Not logging"));
            }
            (None, Some(title)) => {
                self.pane.set_text(&title);
                self.file.set_text("This pane has no output processing");
            }
            (None, None) => {
                self.pane.set_text("No pane");
                self.file.set_text("Open or focus a pane");
            }
        }
        for w in [self.stamps.upcast_ref::<gtk::Widget>(), self.marks.upcast_ref(), self.mode.upcast_ref()] {
            w.set_sensitive(out.is_some());
        }
        self.syncing.set(false);
    }

    fn save(self: &Rc<Self>, rules: Vec<Rule>) {
        if let Err(e) = output::save_rules(&rules) {
            self.toast(&format!("Save rules: {e}"));
        }
        for e in output::set_rules(&rules) {
            self.toast(&format!("Rule skipped: {e}"));
        }
        self.rules.replace(rules);
        self.fill();
    }

    fn edit(self: &Rc<Self>, f: impl FnOnce(&mut Vec<Rule>)) {
        let mut rules = self.rules.borrow().clone();
        f(&mut rules);
        self.save(rules);
    }

    fn fill(self: &Rc<Self>) {
        self.list.remove_all();
        let rules = self.rules.borrow().clone();
        if rules.is_empty() {
            let empty = gtk::Label::new(Some("No rules. Add one with +."));
            empty.add_css_class("dim-label");
            empty.set_margin_top(8);
            self.list.append(&empty);
        }
        for (i, r) in rules.iter().enumerate() {
            self.list.append(&self.row(i, r));
        }
    }

    fn row(self: &Rc<Self>, i: usize, r: &Rule) -> gtk::Box {
        let on = gtk::CheckButton::new();
        on.set_active(r.enabled);
        on.set_tooltip_text(Some("Enabled"));
        let text = gtk::Label::new(Some(&r.pattern));
        text.set_xalign(0.0);
        text.set_hexpand(true);
        text.set_ellipsize(gtk::pango::EllipsizeMode::End);
        text.add_css_class("monospace");
        text.add_css_class(&format!("tint-{}", r.color));
        if r.bold {
            text.add_css_class("heading");
        }
        let flags = gtk::Label::new(Some(if r.ignore_case { "Aa" } else { "" }));
        flags.add_css_class("dim-label");
        flags.set_tooltip_text(Some("Ignores case"));
        let edit = gtk::Button::from_icon_name("document-edit-symbolic");
        edit.add_css_class("flat");
        edit.set_tooltip_text(Some("Edit"));
        let up = gtk::Button::from_icon_name("go-up-symbolic");
        up.add_css_class("flat");
        up.set_tooltip_text(Some("Move up (earlier rules win on overlap)"));
        up.set_sensitive(i > 0);
        let del = gtk::Button::from_icon_name("user-trash-symbolic");
        del.add_css_class("flat");
        del.set_tooltip_text(Some("Delete"));

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        for w in [
            on.upcast_ref::<gtk::Widget>(),
            text.upcast_ref(),
            flags.upcast_ref(),
            up.upcast_ref(),
            edit.upcast_ref(),
            del.upcast_ref(),
        ] {
            row.append(w);
        }

        let me = Rc::downgrade(self);
        on.connect_toggled(move |b| {
            let on = b.is_active();
            if let Some(p) = me.upgrade() {
                p.edit(|rs| {
                    if let Some(r) = rs.get_mut(i) {
                        r.enabled = on;
                    }
                });
            }
        });
        let me = Rc::downgrade(self);
        let rule = r.clone();
        edit.connect_clicked(move |_| {
            if let Some(p) = me.upgrade() {
                editor(&p, Some(i), rule.clone());
            }
        });
        let me = Rc::downgrade(self);
        up.connect_clicked(move |_| {
            if let Some(p) = me.upgrade() {
                p.edit(|rs| {
                    if i > 0 && i < rs.len() {
                        rs.swap(i - 1, i);
                    }
                });
            }
        });
        let me = Rc::downgrade(self);
        del.connect_clicked(move |_| {
            if let Some(p) = me.upgrade() {
                p.edit(|rs| {
                    if i < rs.len() {
                        rs.remove(i);
                    }
                });
            }
        });
        row
    }
}

/// Rule editor. `at` is the index to replace; None appends.
fn editor(p: &Rc<Panel>, at: Option<usize>, init: Rule) {
    let d = adw::AlertDialog::new(Some(if at.is_some() { "Edit Rule" } else { "New Rule" }), None);
    let pattern = gtk::Entry::new();
    pattern.set_text(&init.pattern);
    pattern.set_placeholder_text(Some(r"Regex, e.g. \b(error|fail)\b"));
    pattern.set_activates_default(true);
    pattern.add_css_class("monospace");
    let error = gtk::Label::new(None);
    error.add_css_class("error");
    error.add_css_class("caption");
    error.set_xalign(0.0);
    error.set_wrap(true);
    let names: Vec<String> = COLORS.iter().map(|c| format!("{}{}", c[..1].to_uppercase(), &c[1..])).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let color = gtk::DropDown::from_strings(&refs);
    color.set_selected(COLORS.iter().position(|c| *c == init.color).unwrap_or(0) as u32);
    let bold = gtk::CheckButton::with_label("Bold");
    bold.set_active(init.bold);
    let case = gtk::CheckButton::with_label("Ignore case");
    case.set_active(init.ignore_case);
    let opts = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    opts.append(&color);
    opts.append(&bold);
    opts.append(&case);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
    body.append(&pattern);
    body.append(&error);
    body.append(&opts);
    d.set_extra_child(Some(&body));
    d.add_responses(&[("cancel", "Cancel"), ("save", "Save")]);
    d.set_response_appearance("save", adw::ResponseAppearance::Suggested);
    d.set_default_response(Some("save"));
    d.set_close_response("cancel");

    let rule = {
        let (pattern, color, bold, case) = (pattern.clone(), color.clone(), bold.clone(), case.clone());
        move || Rule {
            pattern: pattern.text().to_string(),
            color: COLORS.get(color.selected() as usize).unwrap_or(&"red").to_string(),
            bold: bold.is_active(),
            ignore_case: case.is_active(),
            enabled: init.enabled,
        }
    };
    let validate = {
        let (d, error, rule) = (d.clone(), error.clone(), rule.clone());
        move || {
            let r = rule();
            let res = if r.pattern.is_empty() { Err(String::new()) } else { output::check(&r) };
            error.set_text(res.as_ref().err().map(String::as_str).unwrap_or(""));
            error.set_visible(res.as_ref().is_err_and(|e| !e.is_empty()));
            d.set_response_enabled("save", res.is_ok());
        }
    };
    validate();
    let v = validate.clone();
    pattern.connect_changed(move |_| v());
    case.connect_toggled(move |_| validate());

    let parent = p.widget.clone();
    let p = p.clone();
    d.connect_response(None, move |_, r| {
        if r != "save" {
            return;
        }
        let new = rule();
        p.edit(|rs| match at {
            Some(i) if i < rs.len() => rs[i] = new,
            _ => rs.push(new),
        });
    });
    d.present(Some(&parent));
}
