// Transfer list shared by all windows, and the Transfers dock page.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

use gtk::{glib, prelude::*};

use crate::transfers::{Cmd, Conflict, Dir, Queue, Update};

#[derive(Clone, PartialEq)]
pub enum Run {
    Queued,
    Running,
    Done(String),
    Failed(String),
}

/// What a transfer was started with, for Retry.
#[derive(Clone)]
struct Spec {
    local: PathBuf,
    remote: String,
    cmd: Cmd,
    force: bool,
}

#[derive(Clone)]
pub struct Item {
    pub id: u64,
    pub name: String,
    pub dir: Dir,
    spec: Spec,
    pub total: u64,
    pub done: u64,
    pub run: Run,
    started: Instant,
}

impl Item {
    fn text(&self) -> String {
        let size = |n: u64| glib::format_size(n).to_string();
        match &self.run {
            Run::Queued => "queued".into(),
            Run::Running => {
                let secs = self.started.elapsed().as_secs_f64().max(0.1);
                let rate = (self.done as f64 / secs) as u64;
                format!("{} of {} · {}/s", size(self.done), size(self.total), size(rate))
            }
            Run::Done(msg) => msg.clone(),
            Run::Failed(msg) => msg.clone(),
        }
    }

    fn fraction(&self) -> f64 {
        match self.run {
            Run::Done(_) => 1.0,
            _ if self.total == 0 => 0.0,
            _ => (self.done as f64 / self.total as f64).min(1.0),
        }
    }

    pub fn finished(&self) -> bool {
        matches!(self.run, Run::Done(_) | Run::Failed(_))
    }
}

type Listener = Box<dyn Fn(u64)>;

pub struct Xfers {
    queue: Queue,
    items: RefCell<Vec<Item>>,
    /// Called with an item id, or 0 when the whole list changed.
    listeners: RefCell<Vec<Listener>>,
}

impl Xfers {
    pub fn new() -> Rc<Self> {
        let (queue, rx) = Queue::start();
        let x = Rc::new(Self { queue, items: RefCell::default(), listeners: RefCell::default() });
        let weak = Rc::downgrade(&x);
        glib::spawn_future_local(async move {
            while let Ok(u) = rx.recv().await {
                let Some(x) = weak.upgrade() else { break };
                x.apply(u);
            }
        });
        x
    }

    fn apply(&self, u: Update) {
        let id = {
            let mut items = self.items.borrow_mut();
            let find = |id: u64| items.iter().position(|i| i.id == id);
            let (id, pos) = match &u {
                Update::Started { id, .. } | Update::Progress { id, .. } | Update::Done { id, .. } | Update::Failed { id, .. } => {
                    (*id, find(*id))
                }
            };
            let Some(pos) = pos else { return };
            let it = &mut items[pos];
            match u {
                Update::Started { total, .. } => {
                    it.total = total;
                    it.run = Run::Running;
                    it.started = Instant::now();
                }
                Update::Progress { done, .. } => it.done = done,
                Update::Done { bytes, skipped, .. } => {
                    it.done = bytes;
                    let secs = it.started.elapsed().as_secs_f64().max(0.1);
                    let mut msg = format!("done · {} in {:.1} s", glib::format_size(bytes), secs);
                    if skipped > 0 {
                        msg.push_str(&format!(" · {skipped} skipped"));
                    }
                    it.run = Run::Done(msg);
                }
                Update::Failed { msg, .. } => it.run = Run::Failed(msg),
            }
            id
        };
        self.notify(id);
    }

    fn notify(&self, id: u64) {
        for f in self.listeners.borrow().iter() {
            f(id);
        }
    }

    pub fn on_change(&self, f: impl Fn(u64) + 'static) {
        self.listeners.borrow_mut().push(Box::new(f));
    }

    /// Queues a transfer; `force` overwrites regardless of the policy.
    pub fn add(&self, dir: Dir, local: PathBuf, remote: String, cmd: Cmd, force: bool) {
        let name = match dir {
            Dir::Up => local.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            Dir::Down => remote.rsplit('/').next().unwrap_or(&remote).to_string(),
        };
        let spec = Spec { local, remote, cmd, force };
        let id = self.queue.add(dir, spec.local.clone(), spec.remote.clone(), spec.cmd.clone(), force);
        let it = Item { id, name, dir, spec, total: 0, done: 0, run: Run::Queued, started: Instant::now() };
        self.items.borrow_mut().push(it);
        self.notify(id);
    }

    /// Runs a failed transfer again; a .part left behind is continued.
    pub fn retry(&self, id: u64) {
        {
            let mut items = self.items.borrow_mut();
            let Some(it) = items.iter_mut().find(|i| i.id == id && matches!(i.run, Run::Failed(_))) else { return };
            let s = it.spec.clone();
            it.id = self.queue.add(it.dir, s.local, s.remote, s.cmd, s.force);
            it.run = Run::Queued;
            it.done = 0;
            it.started = Instant::now();
        }
        // Row ids changed; redraw the list.
        self.notify(0);
    }

    pub fn item(&self, id: u64) -> Option<Item> {
        self.items.borrow().iter().find(|i| i.id == id).cloned()
    }

    pub fn ids(&self) -> Vec<u64> {
        self.items.borrow().iter().map(|i| i.id).collect()
    }

    pub fn cancel(&self, id: u64) {
        self.queue.cancel(id);
    }

    pub fn set_conflict(&self, c: Conflict) {
        self.queue.set_conflict(c);
    }

    pub fn clear_finished(&self) {
        self.items.borrow_mut().retain(|i| !i.finished());
        self.notify(0);
    }
}

struct Row {
    row: gtk::ListBoxRow,
    bar: gtk::ProgressBar,
    info: gtk::Label,
    cancel: gtk::Button,
    retry: gtk::Button,
}

fn make_row(x: &Rc<Xfers>, it: &Item) -> Row {
    let icon = gtk::Image::from_icon_name(match it.dir {
        Dir::Up => "document-send-symbolic",
        Dir::Down => "folder-download-symbolic",
    });
    icon.add_css_class(match it.dir {
        Dir::Up => "tint-magenta",
        Dir::Down => "tint-green",
    });
    let name = gtk::Label::new(Some(&it.name));
    name.set_xalign(0.0);
    name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    name.set_width_chars(24);
    let bar = gtk::ProgressBar::new();
    bar.set_hexpand(true);
    bar.set_valign(gtk::Align::Center);
    let info = gtk::Label::new(None);
    info.set_xalign(0.0);
    info.add_css_class("dim-label");
    info.set_width_chars(34);
    info.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let cancel = gtk::Button::from_icon_name("process-stop-symbolic");
    cancel.add_css_class("flat");
    cancel.set_tooltip_text(Some("Cancel"));
    let (w, id) = (Rc::downgrade(x), it.id);
    cancel.connect_clicked(move |_| {
        if let Some(x) = w.upgrade() {
            x.cancel(id);
        }
    });

    let retry = gtk::Button::from_icon_name("view-refresh-symbolic");
    retry.add_css_class("flat");
    retry.set_tooltip_text(Some("Retry (continues where it stopped)"));
    let (w, id) = (Rc::downgrade(x), it.id);
    retry.connect_clicked(move |_| {
        if let Some(x) = w.upgrade() {
            x.retry(id);
        }
    });

    let b = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    b.set_margin_start(6);
    b.set_margin_end(6);
    b.set_margin_top(2);
    b.set_margin_bottom(2);
    b.append(&icon);
    b.append(&name);
    b.append(&bar);
    b.append(&info);
    b.append(&retry);
    b.append(&cancel);
    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&b));
    Row { row, bar, info, cancel, retry }
}

fn paint(r: &Row, it: &Item) {
    r.bar.set_fraction(it.fraction());
    r.info.set_text(&it.text());
    r.info.set_tooltip_text(Some(&it.text()));
    r.cancel.set_visible(!it.finished());
    r.retry.set_visible(matches!(it.run, Run::Failed(_)));
    match &it.run {
        Run::Failed(_) => r.info.add_css_class("tint-red"),
        _ => r.info.remove_css_class("tint-red"),
    }
}

/// The Transfers dock page for one window.
pub fn page(x: &Rc<Xfers>) -> gtk::Box {
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("transfers");
    let empty = gtk::Label::new(Some("No transfers. Drop files on the Files panel or use its upload button."));
    empty.add_css_class("dim-label");
    list.set_placeholder(Some(&empty));
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_vexpand(true);
    scroll.set_child(Some(&list));

    let labels: Vec<&str> = Conflict::ALL.iter().map(|c| c.label()).collect();
    let policy = gtk::DropDown::from_strings(&labels);
    policy.set_tooltip_text(Some("When the target already exists"));
    let w = Rc::downgrade(x);
    policy.connect_selected_notify(move |d| {
        if let (Some(x), Some(c)) = (w.upgrade(), Conflict::ALL.get(d.selected() as usize)) {
            x.set_conflict(*c);
        }
    });
    let clear = gtk::Button::with_label("Clear Finished");
    clear.add_css_class("flat");
    let w = Rc::downgrade(x);
    clear.connect_clicked(move |_| {
        if let Some(x) = w.upgrade() {
            x.clear_finished();
        }
    });
    let label = gtk::Label::new(Some("If target exists:"));
    label.add_css_class("dim-label");
    let head = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    head.set_margin_start(6);
    head.set_margin_end(6);
    head.set_margin_top(4);
    head.append(&label);
    head.append(&policy);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    head.append(&spacer);
    head.append(&clear);

    let page = gtk::Box::new(gtk::Orientation::Vertical, 4);
    page.append(&head);
    page.append(&scroll);

    let rows: Rc<RefCell<HashMap<u64, Row>>> = Rc::default();
    let render = {
        let (list, rows, w) = (list.downgrade(), rows.clone(), Rc::downgrade(x));
        move |id: u64| {
            let (Some(list), Some(x)) = (list.upgrade(), w.upgrade()) else { return };
            let mut rows = rows.borrow_mut();
            if id == 0 {
                for (_, r) in rows.drain() {
                    list.remove(&r.row);
                }
                for i in x.ids() {
                    if let Some(it) = x.item(i) {
                        let r = make_row(&x, &it);
                        paint(&r, &it);
                        list.append(&r.row);
                        rows.insert(i, r);
                    }
                }
                return;
            }
            let Some(it) = x.item(id) else { return };
            let r = rows.entry(id).or_insert_with(|| {
                let r = make_row(&x, &it);
                list.append(&r.row);
                r
            });
            paint(r, &it);
        }
    };
    render(0);
    x.on_change(render);
    page
}
