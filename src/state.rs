// Process-wide state shared by all windows.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::{gdk, glib, prelude::*};
use vte::prelude::*;

use crate::recent::{self, Recent};
use crate::settings::{self, Settings};
use crate::store::Store;
use crate::term::Launch;
use crate::theme::{self, Theme};
use crate::xfers::Xfers;

pub struct State {
    pub themes: Vec<Theme>,
    pub cfg: RefCell<Settings>,
    pub store: RefCell<Store>,
    pub recent: RefCell<Vec<Recent>>,
    /// Transfer queue and list, shared by all windows.
    pub xfers: Rc<Xfers>,
    listeners: RefCell<Vec<Box<dyn Fn()>>>,
    /// One-time message for the first window (e.g. startup cleanup).
    pub notice: RefCell<Option<String>>,
    terms: RefCell<Vec<(glib::WeakRef<vte::Terminal>, Launch)>>,
    css: gtk::CssProvider,
}

impl State {
    pub fn new() -> Rc<Self> {
        let css = gtk::CssProvider::new();
        let base = gtk::CssProvider::new();
        base.load_from_string(include_str!("style.css"));
        if let Some(display) = gdk::Display::default() {
            for p in [&base, &css] {
                gtk::style_context_add_provider_for_display(
                    &display,
                    p,
                    gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
                );
            }
        }
        let state = Rc::new(Self {
            themes: theme::load(&settings::dir().join("themes")),
            cfg: RefCell::new(Settings::load()),
            store: RefCell::new(Store::load()),
            recent: RefCell::new(recent::load(recent::RECENT)),
            xfers: Xfers::new(),
            listeners: RefCell::new(Vec::new()),
            notice: RefCell::new(None),
            terms: RefCell::new(Vec::new()),
            css,
        });
        state.apply();
        state
    }

    /// Active theme, falling back to the first one.
    pub fn theme(&self) -> Option<&Theme> {
        let name = self.cfg.borrow().theme.clone();
        self.themes
            .iter()
            .find(|t| t.name == name)
            .or_else(|| self.themes.first())
    }

    pub fn set_theme(&self, name: &str) {
        self.cfg.borrow_mut().theme = name.to_string();
        if let Err(e) = self.cfg.borrow().save() {
            crate::warn(&format!("save settings: {e}"));
        }
        self.apply();
    }

    /// Calls `f` whenever sessions change.
    pub fn on_sessions(&self, f: impl Fn() + 'static) {
        self.listeners.borrow_mut().push(Box::new(f));
    }

    /// Reloads sessions from disk and notifies listeners.
    pub fn reload_sessions(&self) {
        self.store.replace(Store::load());
        self.notify();
    }

    /// Applies `f` to the store, saves and notifies listeners.
    /// On save failure the change stays in memory and the error is returned.
    pub fn edit_sessions(&self, f: impl FnOnce(&mut Store) -> Result<(), String>) -> Result<(), String> {
        f(&mut self.store.borrow_mut())?;
        let saved = self.store.borrow().save().map_err(|e| format!("save sessions: {e}"));
        self.notify();
        saved
    }

    /// Puts `l` on top of the recent list.
    pub fn remember(&self, l: &Launch) {
        recent::push(&mut self.recent.borrow_mut(), Recent::of(l));
        if let Err(e) = recent::save(recent::RECENT, &self.recent.borrow()) {
            crate::warn(&format!("save recent: {e}"));
        }
        self.notify();
    }

    fn notify(&self) {
        for f in self.listeners.borrow().iter() {
            f();
        }
    }

    pub fn set_restore(&self, on: bool) {
        self.cfg.borrow_mut().restore_tabs = on;
        if let Err(e) = self.cfg.borrow().save() {
            crate::warn(&format!("save settings: {e}"));
        }
    }

    /// Registers a terminal so theme changes reach it.
    pub fn track(&self, term: &vte::Terminal, launch: &Launch) {
        let mut terms = self.terms.borrow_mut();
        terms.retain(|(w, _)| w.upgrade().is_some());
        terms.push((term.downgrade(), launch.clone()));
        if let Some(t) = self.theme() {
            paint(term, t);
        }
    }

    /// What `term` was launched with.
    pub fn launch(&self, term: &vte::Terminal) -> Option<Launch> {
        self.terms
            .borrow()
            .iter()
            .find(|(w, _)| w.upgrade().as_ref() == Some(term))
            .map(|(_, l)| l.clone())
    }

    fn apply(&self) {
        let Some(t) = self.theme() else {
            return;
        };
        let scheme = if t.dark {
            adw::ColorScheme::ForceDark
        } else {
            adw::ColorScheme::ForceLight
        };
        adw::StyleManager::default().set_color_scheme(scheme);
        self.css.load_from_string(&t.css);

        let mut terms = self.terms.borrow_mut();
        terms.retain(|(w, _)| w.upgrade().is_some());
        for term in terms.iter().filter_map(|(w, _)| w.upgrade()) {
            paint(&term, t);
        }
    }
}

fn paint(term: &vte::Terminal, t: &Theme) {
    let c = &t.colors;
    let palette: Vec<&gdk::RGBA> = c.palette.iter().collect();
    term.set_colors(Some(&c.fg), Some(&c.bg), &palette);
    term.set_color_cursor(Some(&c.cursor));
    term.set_color_highlight(Some(&c.selection));
}
