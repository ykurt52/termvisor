// Split pane tree inside a tab.
//
// Layout: tab root (Box) -> Paned* -> leaf (ScrolledWindow) -> Terminal.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::{glib, prelude::*};
use vte::prelude::*;

use crate::{logcat, multi, output, plot, streampane, toolpane, usbpane};
use crate::state::State;
use crate::term::{self, Kind, Launch};

const ROOT_CLASS: &str = "tab-root";

/// Where a detached widget used to live.
enum Slot {
    Start(gtk::Paned),
    End(gtk::Paned),
    Root(gtk::Box),
}

fn take(w: &gtk::Widget) -> Option<Slot> {
    let parent = w.parent()?;
    if let Some(p) = parent.downcast_ref::<gtk::Paned>() {
        if p.start_child().as_ref() == Some(w) {
            p.set_start_child(None::<&gtk::Widget>);
            return Some(Slot::Start(p.clone()));
        }
        p.set_end_child(None::<&gtk::Widget>);
        return Some(Slot::End(p.clone()));
    }
    let b = parent.downcast::<gtk::Box>().ok()?;
    b.remove(w);
    Some(Slot::Root(b))
}

fn put(slot: &Slot, w: &impl IsA<gtk::Widget>) {
    match slot {
        Slot::Start(p) => p.set_start_child(Some(w)),
        Slot::End(p) => p.set_end_child(Some(w)),
        Slot::Root(b) => b.append(w),
    }
}

pub fn root() -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 0);
    b.add_css_class(ROOT_CLASS);
    b.set_hexpand(true);
    b.set_vexpand(true);
    b
}

fn root_of(w: &gtk::Widget) -> Option<gtk::Box> {
    let mut cur = w.parent();
    while let Some(p) = cur {
        if p.has_css_class(ROOT_CLASS) {
            return p.downcast().ok();
        }
        cur = p.parent();
    }
    None
}

/// Tab view and page that hold `w`.
pub fn page_of(w: &gtk::Widget) -> Option<(adw::TabView, adw::TabPage)> {
    let root = root_of(w)?;
    let view: adw::TabView = root.ancestor(adw::TabView::static_type())?.downcast().ok()?;
    let page = view.page(&root);
    Some((view, page))
}

/// First terminal found under `w`.
pub fn find(w: &gtk::Widget) -> Option<vte::Terminal> {
    if let Some(t) = w.downcast_ref::<vte::Terminal>() {
        return Some(t.clone());
    }
    let mut child = w.first_child();
    while let Some(c) = child {
        if let Some(t) = find(&c) {
            return Some(t);
        }
        child = c.next_sibling();
    }
    None
}

const HOST_KEY_BANNER: &str = "\r\n\x1b[1;37;41m HOST KEY CHANGED \x1b[0m\x1b[1;31m The server key differs from known_hosts. \
This can be a reinstalled server or an attack. Verify the new fingerprint out of band, \
then remove the old key with: ssh-keygen -R <host>\x1b[0m\r\n";

/// True when ssh reported a changed host key on screen.
fn host_key_changed(t: &vte::Terminal) -> bool {
    t.text_format(vte::Format::Text)
        .is_some_and(|s| s.contains("REMOTE HOST IDENTIFICATION HAS CHANGED"))
}

const LEAF_CLASS: &str = "leaf";

/// Pane widget that holds `term` (its direct child of a Paned or root).
fn leaf_of(term: &vte::Terminal) -> Option<gtk::Widget> {
    let mut cur = term.parent();
    while let Some(w) = cur {
        if w.has_css_class(LEAF_CLASS) {
            return Some(w);
        }
        cur = w.parent();
    }
    None
}

/// Creates a pane for `launch`. Returns the terminal and the leaf widget
/// to insert into the layout.
pub fn open(state: &Rc<State>, launch: &Launch) -> (vte::Terminal, gtk::Widget) {
    let (t, leaf) = match &launch.stream {
        Some(s) => streampane::build(state, launch, s.clone()),
        None if launch.kind == Kind::Logcat => logcat::build(state, launch),
        None if launch.kind == Kind::Usb => usbpane::build(state, launch),
        None if launch.kind == Kind::Tool => toolpane::build(state, launch),
        None => {
            let t = hooked(state, launch);
            let leaf = term::scroll(&t).upcast::<gtk::Widget>();
            (t, leaf)
        }
    };
    let leaf = multi::wrap(&t, &leaf).upcast::<gtk::Widget>();
    leaf.add_css_class(LEAF_CLASS);
    leaf.set_hexpand(true);
    leaf.set_vexpand(true);
    (t, leaf)
}

/// Terminal with title and exit hooks.
///
/// A local shell closes its pane on exit. Other sessions stay open and
/// restart on Enter.
fn hooked(state: &Rc<State>, launch: &Launch) -> vte::Terminal {
    let t = term::spawn(state, launch);
    t.connect_window_title_changed(|t| {
        let Some(title) = t.window_title() else {
            return;
        };
        if let Some((_, page)) = page_of(t.upcast_ref()) {
            page.set_title(&title);
        }
    });

    if launch.kind == Kind::Shell {
        t.connect_child_exited(|t, _| {
            let t = t.clone();
            glib::idle_add_local_once(move || close(&t));
        });
        return t;
    }

    let dead = Rc::new(Cell::new(false));
    let flag = dead.clone();
    t.connect_child_exited(move |t, status| {
        flag.set(true);
        if host_key_changed(t) {
            t.feed(HOST_KEY_BANNER.as_bytes());
        }
        let msg = format!(
            "\r\n\x1b[33m[session ended, status {status}] Enter: reconnect, Ctrl+Shift+W: close\x1b[0m\r\n"
        );
        t.feed(msg.as_bytes());
    });
    let again = launch.clone();
    t.connect_commit(move |t, text, _| {
        if dead.get() && text.contains('\r') {
            dead.set(false);
            term::run(t, &again);
        }
    });
    t
}

thread_local! {
    /// Plots shown above panes.
    static PLOTS: RefCell<Vec<(glib::WeakRef<vte::Terminal>, Rc<plot::Plot>)>> = const { RefCell::new(Vec::new()) };
}

const PLOT_SPLIT: &str = "plot-split";

/// Shows or hides a live plot of the numbers `term` prints, above it.
pub fn toggle_plot(term: &vte::Terminal) {
    let Some(overlay) = leaf_of(term).and_downcast::<gtk::Overlay>() else { return };
    let Some(child) = overlay.child() else { return };
    if let Some(split) = child.downcast_ref::<gtk::Paned>().filter(|p| p.has_css_class(PLOT_SPLIT)) {
        let content = split.end_child();
        split.set_end_child(None::<&gtk::Widget>);
        overlay.set_child(content.as_ref());
        PLOTS.with(|v| v.borrow_mut().retain(|(w, _)| w.upgrade().is_some_and(|t| &t != term)));
        term.grab_focus();
        return;
    }
    let Some(out) = output::of(term) else { return };
    let p = plot::build(&out);
    let split = gtk::Paned::new(gtk::Orientation::Vertical);
    split.add_css_class(PLOT_SPLIT);
    split.set_wide_handle(true);
    overlay.set_child(None::<&gtk::Widget>);
    split.set_start_child(Some(&p.widget));
    split.set_end_child(Some(&child));
    split.set_position((overlay.height() * 45 / 100).max(220));
    overlay.set_child(Some(&split));
    PLOTS.with(|v| {
        let mut v = v.borrow_mut();
        v.retain(|(w, _)| w.upgrade().is_some());
        v.push((term.downgrade(), p));
    });
    term.grab_focus();
}

/// Splits the pane of `term`, new terminal goes right or below.
pub fn split(state: &Rc<State>, term: &vte::Terminal, dir: gtk::Orientation) -> Option<vte::Terminal> {
    let leaf = leaf_of(term)?;
    let size = match dir {
        gtk::Orientation::Horizontal => leaf.width(),
        _ => leaf.height(),
    };
    let slot = take(&leaf)?;

    let paned = gtk::Paned::new(dir);
    paned.set_wide_handle(true);
    paned.set_hexpand(true);
    paned.set_vexpand(true);
    let (fresh, fresh_leaf) = open(state, &Launch::shell(term::cwd(term)));
    paned.set_start_child(Some(&leaf));
    paned.set_end_child(Some(&fresh_leaf));
    if size > 0 {
        paned.set_position(size / 2);
    }
    put(&slot, &paned);
    fresh.grab_focus();
    Some(fresh)
}

/// Closes the pane of `term`; closes the tab when it was the last pane.
pub fn close(term: &vte::Terminal) {
    let Some(leaf) = leaf_of(term) else {
        return;
    };
    let owner = page_of(leaf.upcast_ref());
    let Some(parent) = leaf.parent() else {
        return;
    };

    if let Some(p) = parent.downcast_ref::<gtk::Paned>() {
        let sibling = if p.start_child().as_ref() == Some(&leaf) {
            p.end_child()
        } else {
            p.start_child()
        };
        p.set_start_child(None::<&gtk::Widget>);
        p.set_end_child(None::<&gtk::Widget>);
        let (Some(slot), Some(sibling)) = (take(p.upcast_ref()), sibling) else {
            return;
        };
        put(&slot, &sibling);
        if let Some(t) = find(&sibling) {
            t.grab_focus();
        }
        return;
    }

    if let Some((view, page)) = owner {
        view.close_page(&page);
    }
}
