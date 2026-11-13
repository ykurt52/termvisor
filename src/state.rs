// Process-wide state shared by all windows.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::{gdk, glib, prelude::*};

use crate::commands::{self, Page};
use crate::recent::{self, Recent};
use crate::settings::{self, Settings};
use crate::store::Store;
use crate::term::Launch;
use crate::theme::{self, Theme};
use crate::xfers::Xfers;

type TermFn = Box<dyn Fn(&crate::vt::View)>;

pub struct State {
    themes: RefCell<Vec<Theme>>,
    /// Theme shown while the editor changes it.
    preview: RefCell<Option<Theme>>,
    theme_listeners: RefCell<Vec<Box<dyn Fn()>>>,
    /// Watches the user theme folder.
    monitor: RefCell<Option<gtk::gio::FileMonitor>>,
    pub cfg: RefCell<Settings>,
    pub store: RefCell<Store>,
    pub recent: RefCell<Vec<Recent>>,
    /// Transfer queue and list, shared by all windows.
    pub xfers: Rc<Xfers>,
    /// SSH tunnels, shared by all windows.
    pub tunnels: Rc<crate::tunnels::Manager>,
    /// Command panel pages, shared by all windows.
    pub commands: RefCell<Vec<Page>>,
    cmd_listeners: RefCell<Vec<Box<dyn Fn()>>>,
    listeners: RefCell<Vec<Box<dyn Fn()>>>,
    /// Called when a terminal's shell reports a new directory (OSC 7).
    cwd_listeners: RefCell<Vec<TermFn>>,
    /// One-time message for the first window (e.g. startup cleanup).
    pub notice: RefCell<Option<String>>,
    terms: RefCell<Vec<(glib::WeakRef<crate::vt::View>, Launch)>>,
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
            themes: RefCell::new(theme::load(&theme_dir())),
            preview: RefCell::new(None),
            theme_listeners: RefCell::new(Vec::new()),
            monitor: RefCell::new(None),
            cfg: RefCell::new(Settings::load()),
            store: RefCell::new(Store::load()),
            recent: RefCell::new(recent::load(recent::RECENT)),
            xfers: Xfers::new(),
            commands: RefCell::new(commands::load()),
            tunnels: crate::tunnels::Manager::new(),
            cmd_listeners: RefCell::new(Vec::new()),
            listeners: RefCell::new(Vec::new()),
            notice: RefCell::new(None),
            cwd_listeners: RefCell::new(Vec::new()),
            terms: RefCell::new(Vec::new()),
            css,
        });
        state.apply();
        state.watch_themes();
        // Following the system: its style may change any time.
        let w = Rc::downgrade(&state);
        adw::StyleManager::default().connect_dark_notify(move |_| {
            if let Some(s) = w.upgrade() {
                if s.cfg.borrow().follow_system && s.preview.borrow().is_none() {
                    s.apply();
                }
            }
        });
        state
    }

    /// Reloads user themes when their folder changes (a short delay
    /// collects the events of one save).
    fn watch_themes(self: &Rc<Self>) {
        let dir = theme_dir();
        let _ = std::fs::create_dir_all(&dir);
        let Ok(m) = gtk::gio::File::for_path(&dir).monitor_directory(gtk::gio::FileMonitorFlags::NONE, None::<&gtk::gio::Cancellable>) else { return };
        let w = Rc::downgrade(self);
        let pending = Rc::new(std::cell::Cell::new(false));
        m.connect_changed(move |_, _, _, _| {
            if pending.replace(true) {
                return;
            }
            let (w, p) = (w.clone(), pending.clone());
            glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
                p.set(false);
                if let Some(s) = w.upgrade() {
                    s.reload_themes();
                }
            });
        });
        self.monitor.replace(Some(m));
    }

    pub fn themes(&self) -> Vec<Theme> {
        self.themes.borrow().clone()
    }

    fn named(&self, name: &str) -> Option<Theme> {
        self.themes.borrow().iter().find(|t| t.name == name).cloned()
    }

    /// Name of the chosen theme (after the system style when following it).
    fn chosen(&self) -> String {
        let c = self.cfg.borrow();
        if !c.follow_system {
            return c.theme.clone();
        }
        if adw::StyleManager::default().is_dark() { c.theme_dark.clone() } else { c.theme_light.clone() }
    }

    /// Active theme, falling back to the first one.
    pub fn theme(&self) -> Option<Theme> {
        if let Some(p) = self.preview.borrow().clone() {
            return Some(p);
        }
        self.named(&self.chosen()).or_else(|| self.themes.borrow().first().cloned())
    }

    /// Theme of a pane: a matching rule's, else the active one.
    fn theme_for(&self, l: &Launch) -> Option<Theme> {
        let rule = self.cfg.borrow().theme_rules.iter().find(|r| r.matches(&l.title, &l.target)).map(|r| r.theme.clone());
        rule.and_then(|n| self.named(&n)).or_else(|| self.theme())
    }

    pub fn set_theme(&self, name: &str) {
        self.edit_cfg(|c| {
            c.theme = name.to_string();
            c.follow_system = false;
        });
        self.apply();
        self.themes_changed();
    }

    /// Shows `t` everywhere until called with None (theme editor).
    pub fn preview(&self, t: Option<Theme>) {
        self.preview.replace(t);
        self.apply();
    }

    pub fn reload_themes(&self) {
        self.themes.replace(theme::load(&theme_dir()));
        self.apply();
        self.themes_changed();
    }

    /// Calls `f` when the theme list or the chosen theme changes.
    pub fn on_themes(&self, f: impl Fn() + 'static) {
        self.theme_listeners.borrow_mut().push(Box::new(f));
    }

    fn themes_changed(&self) {
        for f in self.theme_listeners.borrow().iter() {
            f();
        }
    }

    /// Applies theme settings (follow system, rules) after they changed.
    pub fn retheme(&self) {
        self.apply();
        self.themes_changed();
    }

    /// Font and scrollback from settings, on every open terminal.
    pub fn apply_terminal_prefs(&self) {
        let (font, lines) = {
            let c = self.cfg.borrow();
            (gtk::pango::FontDescription::from_string(&c.font), c.scrollback)
        };
        for term in self.terms.borrow().iter().filter_map(|(w, _)| w.upgrade()) {
            term.set_font(Some(&font));
            term.set_scrollback_lines(lines as _);
        }
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

    /// Removes entry `i` of the recent list; `None` clears it.
    pub fn forget_recent(&self, i: Option<usize>) {
        {
            let mut list = self.recent.borrow_mut();
            match i {
                Some(i) if i < list.len() => {
                    list.remove(i);
                }
                Some(_) => return,
                None => list.clear(),
            }
        }
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

    /// Calls `f` whenever command pages change.
    pub fn on_commands(&self, f: impl Fn() + 'static) {
        self.cmd_listeners.borrow_mut().push(Box::new(f));
    }

    /// Applies `f` to the command pages, saves and notifies listeners.
    pub fn edit_commands(&self, f: impl FnOnce(&mut Vec<Page>)) -> Result<(), String> {
        f(&mut self.commands.borrow_mut());
        let saved = commands::save(&self.commands.borrow()).map_err(|e| format!("save commands: {e}"));
        for f in self.cmd_listeners.borrow().iter() {
            f();
        }
        saved
    }

    pub fn set_restore(&self, on: bool) {
        self.edit_cfg(|c| c.restore_tabs = on);
    }

    /// Changes settings and saves them.
    pub fn edit_cfg(&self, f: impl FnOnce(&mut Settings)) {
        f(&mut self.cfg.borrow_mut());
        if let Err(e) = self.cfg.borrow().save() {
            crate::warn(&format!("save settings: {e}"));
        }
    }

    /// Tells command panels to redraw (e.g. after a key setting changed).
    pub fn commands_changed(&self) {
        for f in self.cmd_listeners.borrow().iter() {
            f();
        }
    }

    pub fn on_cwd(&self, f: impl Fn(&crate::vt::View) + 'static) {
        self.cwd_listeners.borrow_mut().push(Box::new(f));
    }

    pub fn cwd_changed(&self, t: &crate::vt::View) {
        for f in self.cwd_listeners.borrow().iter() {
            f(t);
        }
    }

    /// Registers a terminal so theme changes reach it.
    pub fn track(&self, term: &crate::vt::View, launch: &Launch) {
        let mut terms = self.terms.borrow_mut();
        terms.retain(|(w, _)| w.upgrade().is_some());
        terms.push((term.downgrade(), launch.clone()));
        drop(terms);
        if let Some(t) = self.theme_for(launch) {
            paint(term, &t);
        }
    }

    /// What `term` was launched with.
    pub fn launch(&self, term: &crate::vt::View) -> Option<Launch> {
        self.terms
            .borrow()
            .iter()
            .find(|(w, _)| w.upgrade().as_ref() == Some(term))
            .map(|(_, l)| l.clone())
    }

    fn apply(&self) {
        let follow = self.cfg.borrow().follow_system && self.preview.borrow().is_none();
        let sm = adw::StyleManager::default();
        // Following the system needs its own style, so the scheme is not forced.
        if follow {
            sm.set_color_scheme(adw::ColorScheme::Default);
        }
        let Some(t) = self.theme() else {
            return;
        };
        if !follow {
            sm.set_color_scheme(if t.dark { adw::ColorScheme::ForceDark } else { adw::ColorScheme::ForceLight });
        }
        self.css.load_from_string(&t.css);

        let terms: Vec<(crate::vt::View, Launch)> = {
            let mut terms = self.terms.borrow_mut();
            terms.retain(|(w, _)| w.upgrade().is_some());
            terms.iter().filter_map(|(w, l)| w.upgrade().map(|t| (t, l.clone()))).collect()
        };
        for (term, l) in terms {
            if let Some(x) = self.theme_for(&l) {
                paint(&term, &x);
            }
        }
    }
}

pub fn theme_dir() -> std::path::PathBuf {
    settings::dir().join("themes")
}

fn paint(term: &crate::vt::View, t: &Theme) {
    let c = &t.colors;
    let palette: Vec<&gdk::RGBA> = c.palette.iter().collect();
    term.set_colors(Some(&c.fg), Some(&c.bg), &palette);
    term.set_color_cursor(Some(&c.cursor));
    term.set_color_highlight(Some(&c.selection));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::ThemeRule;

    /// Everything that needs GTK, in one test: GTK runs on one thread.
    /// Settings are changed in memory only; nothing is saved.
    #[test]
    fn with_gtk() {
        if gtk::init().is_err() || adw::init().is_err() {
            return;
        }
        crate::keybindings::tests::check();

        let st = State::new();
        {
            let mut c = st.cfg.borrow_mut();
            c.theme = "Dark".into();
            c.follow_system = false;
            c.theme_rules = vec![ThemeRule { sessions: "root@prod*, /dev/ttyUSB*".into(), theme: "Nord".into() }];
        }
        st.retheme();
        let prod = Launch::ssh(vec!["root@prod1".into()], "root@prod1");
        assert_eq!(st.theme_for(&prod).map(|t| t.name), Some("Nord".into()));
        assert_eq!(st.theme_for(&Launch::shell(None)).map(|t| t.name), Some("Dark".into()));
        assert!(!adw::StyleManager::default().is_dark() || st.theme().is_some_and(|t| t.dark));

        // Preview shows a theme everywhere until cleared.
        let mut f = st.theme().expect("theme").file;
        f.name = "Preview".into();
        st.preview(Some(theme::build(f).expect("build")));
        assert_eq!(st.theme().map(|t| t.name), Some("Preview".into()));
        st.preview(None);
        assert_eq!(st.theme().map(|t| t.name), Some("Dark".into()));

        // Following the system picks the light or dark theme.
        {
            let mut c = st.cfg.borrow_mut();
            c.follow_system = true;
            c.theme_light = "Solarized Light".into();
            c.theme_dark = "One Dark".into();
        }
        st.retheme();
        let want = if adw::StyleManager::default().is_dark() { "One Dark" } else { "Solarized Light" };
        assert_eq!(st.theme().map(|t| t.name), Some(want.into()));
        assert_eq!(st.themes().iter().filter(|t| t.path.is_none()).count(), 9);
    }
}
