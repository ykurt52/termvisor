// Multi-exec: while it is on, input typed in a selected pane also goes to
// every other selected pane. Selected panes get a red frame.

use std::cell::{Cell, RefCell};
use std::sync::OnceLock;

use gtk::{glib, prelude::*};
use regex::Regex;

use crate::{pty, streampane};

struct Mark {
    term: glib::WeakRef<vte::Terminal>,
    check: glib::WeakRef<gtk::CheckButton>,
    frame: glib::WeakRef<gtk::Box>,
}

thread_local! {
    static ON: Cell<bool> = const { Cell::new(false) };
    static TARGETS: RefCell<Vec<glib::WeakRef<vte::Terminal>>> = const { RefCell::new(Vec::new()) };
    static MARKS: RefCell<Vec<Mark>> = const { RefCell::new(Vec::new()) };
    static LISTENERS: RefCell<Vec<Box<dyn Fn()>>> = const { RefCell::new(Vec::new()) };
}

pub fn on() -> bool {
    ON.get()
}

/// Calls `f` when the mode or the selection changes.
pub fn on_change(f: impl Fn() + 'static) {
    LISTENERS.with(|l| l.borrow_mut().push(Box::new(f)));
}

fn changed() {
    MARKS.with(|m| {
        let mut m = m.borrow_mut();
        m.retain(|x| x.term.upgrade().is_some());
        for x in m.iter() {
            let picked = x.term.upgrade().is_some_and(|t| is_target(&t));
            if let Some(c) = x.check.upgrade() {
                c.set_visible(on());
                if c.is_active() != picked {
                    c.set_active(picked);
                }
            }
            if let Some(f) = x.frame.upgrade() {
                f.set_visible(on() && picked);
            }
        }
    });
    // Listeners may read the state but not register new ones.
    LISTENERS.with(|l| {
        for f in l.borrow().iter() {
            f();
        }
    });
}

pub fn set_on(v: bool) {
    if ON.replace(v) != v {
        changed();
    }
}

pub fn is_target(t: &vte::Terminal) -> bool {
    TARGETS.with(|v| v.borrow().iter().any(|w| w.upgrade().as_ref() == Some(t)))
}

pub fn select(t: &vte::Terminal, pick: bool) {
    let was = is_target(t);
    TARGETS.with(|v| {
        let mut v = v.borrow_mut();
        v.retain(|w| w.upgrade().is_some_and(|x| &x != t));
        if pick {
            v.push(t.downgrade());
        }
    });
    if was != pick {
        changed();
    }
}

/// Selects every open pane, or none.
pub fn select_all(pick: bool) {
    let all: Vec<vte::Terminal> = MARKS.with(|m| m.borrow().iter().filter_map(|x| x.term.upgrade()).collect());
    TARGETS.with(|v| {
        let mut v = v.borrow_mut();
        v.clear();
        if pick {
            v.extend(all.iter().map(|t| t.downgrade()));
        }
    });
    changed();
}

/// Selected panes that are still open.
pub fn targets() -> Vec<vte::Terminal> {
    TARGETS.with(|v| v.borrow().iter().filter_map(|w| w.upgrade()).collect())
}

/// Wraps a pane: a checkbox to select it and a red frame when selected.
pub fn wrap(t: &vte::Terminal, leaf: &gtk::Widget) -> gtk::Overlay {
    let o = gtk::Overlay::new();
    o.set_child(Some(leaf));
    let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
    frame.add_css_class("multi-frame");
    frame.set_can_target(false);
    frame.set_visible(false);
    o.add_overlay(&frame);
    let check = gtk::CheckButton::with_label("Multi-exec");
    check.add_css_class("multi-check");
    check.set_halign(gtk::Align::End);
    check.set_valign(gtk::Align::Start);
    check.set_focus_on_click(false);
    check.set_visible(on());
    let w = t.downgrade();
    check.connect_toggled(move |c| {
        if let Some(t) = w.upgrade() {
            select(&t, c.is_active());
        }
    });
    o.add_overlay(&check);
    MARKS.with(|m| m.borrow_mut().push(Mark { term: t.downgrade(), check: check.downgrade(), frame: frame.downgrade() }));
    o
}

/// Replies the terminal sends by itself (cursor position, device
/// attributes, color queries, focus events); never copied to other panes.
fn is_reply(text: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^(\x1b\[\d+;\d+R|\x1b\[[?>=][\d;]*c|\x1b\[\??[\d;]*\$y|\x1b\[[\d;]*t|\x1b\[[IO]|\x1b\].*|\x1bP.*)$").expect("reply pattern")
    })
    .is_match(text)
}

/// Text typed in `from`: recorded for a macro and copied to the other
/// selected panes.
pub fn typed(from: &vte::Terminal, text: &str) {
    if is_reply(text) {
        return;
    }
    crate::macros::recorded(from, text);
    if !on() || !is_target(from) {
        return;
    }
    for t in targets().iter().filter(|t| *t != from) {
        type_into(t, text);
    }
}

/// Sends `text` as if typed: stream panes map Enter to their line ending
/// and echo; shells get it as is.
pub fn type_into(t: &vte::Terminal, text: &str) {
    if !streampane::type_text(t, text) {
        pty::send(t, text.as_bytes());
    }
}

/// Sends raw bytes (command buttons): stream panes write them to the link
/// unchanged.
pub fn deliver(t: &vte::Terminal, bytes: &[u8], hex: bool) {
    if !streampane::write(t, bytes, hex) {
        pty::send(t, bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::is_reply;

    #[test]
    fn replies_are_not_typing() {
        for r in ["\x1b[12;40R", "\x1b[?62;22c", "\x1b[>1;7600;0c", "\x1b[I", "\x1b[O", "\x1b]11;rgb:0000/0000/0000\x1b\\", "\x1bP1$r0m\x1b\\"] {
            assert!(is_reply(r), "{r:?}");
        }
        for k in ["ls\r", "\x1b[A", "\x1b[15~", "\x1bOP", "\x03", "q"] {
            assert!(!is_reply(k), "{k:?}");
        }
    }
}
