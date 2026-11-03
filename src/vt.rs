// Terminal view drawn with GTK: alacritty_terminal keeps the screen state,
// this widget draws it with Pango and turns keys and mouse into input.
// Works on every platform GTK runs on.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Direction, Line, Point};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Processor, Rgb};
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, pango, prelude::*};

/// Screen size handed to the emulator.
struct Size {
    cols: usize,
    rows: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// Collects emulator events; handled after each feed.
#[derive(Clone, Default)]
struct Events(Rc<RefCell<Vec<Event>>>);

impl EventListener for Events {
    fn send_event(&self, e: Event) {
        self.0.borrow_mut().push(e);
    }
}

#[derive(Clone)]
struct Palette {
    fg: gdk::RGBA,
    bg: gdk::RGBA,
    cursor: Option<gdk::RGBA>,
    highlight: Option<gdk::RGBA>,
    colors: [gdk::RGBA; 16],
}

impl Default for Palette {
    fn default() -> Self {
        const TANGO: [&str; 16] = [
            "#2e3436", "#cc0000", "#4e9a06", "#c4a000", "#3465a4", "#75507b", "#06989a", "#d3d7cf", "#555753", "#ef2929", "#8ae234",
            "#fce94f", "#729fcf", "#ad7fa8", "#34e2e2", "#eeeeec",
        ];
        Palette {
            fg: gdk::RGBA::parse("#d3d7cf").expect("color"),
            bg: gdk::RGBA::parse("#1e1e1e").expect("color"),
            cursor: None,
            highlight: None,
            colors: TANGO.map(|c| gdk::RGBA::parse(c).expect("color")),
        }
    }
}

fn rgba(c: Rgb) -> gdk::RGBA {
    gdk::RGBA::new(f32::from(c.r) / 255.0, f32::from(c.g) / 255.0, f32::from(c.b) / 255.0, 1.0)
}

fn dim(c: gdk::RGBA) -> gdk::RGBA {
    gdk::RGBA::new(c.red() * 0.66, c.green() * 0.66, c.blue() * 0.66, c.alpha())
}

/// Colors 16..255 of the xterm palette.
fn xterm(i: u8) -> gdk::RGBA {
    let i = usize::from(i);
    if i >= 232 {
        let v = (8 + 10 * (i - 232)) as f32 / 255.0;
        return gdk::RGBA::new(v, v, v, 1.0);
    }
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let i = i - 16;
    rgba(Rgb { r: LEVELS[i / 36], g: LEVELS[(i / 6) % 6], b: LEVELS[i % 6] })
}

impl Palette {
    fn indexed(&self, i: usize, set: &Colors) -> gdk::RGBA {
        if let Some(c) = set[i] {
            return rgba(c);
        }
        match i {
            0..=15 => self.colors[i],
            _ => xterm(i as u8),
        }
    }

    fn color(&self, c: Color, set: &Colors) -> gdk::RGBA {
        match c {
            Color::Spec(rgb) => rgba(rgb),
            Color::Indexed(i) => self.indexed(usize::from(i), set),
            Color::Named(n) => {
                if let Some(c) = set[n] {
                    return rgba(c);
                }
                let i = n as usize;
                match n {
                    NamedColor::Foreground | NamedColor::BrightForeground => self.fg,
                    NamedColor::Background => self.bg,
                    NamedColor::Cursor => self.cursor.unwrap_or(self.fg),
                    NamedColor::DimForeground => dim(self.fg),
                    _ if i < 16 => self.colors[i],
                    _ => dim(self.colors[i - NamedColor::DimBlack as usize]),
                }
            }
        }
    }
}

/// Bytes for a special key; None leaves the key to the application.
fn key_bytes(key: gdk::Key, m: gdk::ModifierType, mode: TermMode) -> Option<Vec<u8>> {
    let shift = m.contains(gdk::ModifierType::SHIFT_MASK);
    let alt = m.contains(gdk::ModifierType::ALT_MASK);
    let ctrl = m.contains(gdk::ModifierType::CONTROL_MASK);
    let code = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
    let app = mode.contains(TermMode::APP_CURSOR);
    // CSI sequences with a final letter (arrows, Home, End).
    let letter = |c: char| -> Vec<u8> {
        if code > 1 {
            format!("\x1b[1;{code}{c}").into_bytes()
        } else if app {
            format!("\x1bO{c}").into_bytes()
        } else {
            format!("\x1b[{c}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if code > 1 {
            format!("\x1b[{n};{code}~").into_bytes()
        } else {
            format!("\x1b[{n}~").into_bytes()
        }
    };
    let ss3 = |c: char| -> Vec<u8> {
        if code > 1 {
            format!("\x1b[1;{code}{c}").into_bytes()
        } else {
            format!("\x1bO{c}").into_bytes()
        }
    };
    let esc = |b: &[u8]| -> Vec<u8> {
        let mut v = if alt { vec![0x1b] } else { Vec::new() };
        v.extend_from_slice(b);
        v
    };
    Some(match key {
        gdk::Key::Return | gdk::Key::KP_Enter => esc(b"\r"),
        gdk::Key::BackSpace => esc(if ctrl { b"\x08" } else { b"\x7f" }),
        gdk::Key::Tab => esc(b"\t"),
        gdk::Key::ISO_Left_Tab => b"\x1b[Z".to_vec(),
        gdk::Key::Escape => esc(b"\x1b"),
        gdk::Key::Up | gdk::Key::KP_Up => letter('A'),
        gdk::Key::Down | gdk::Key::KP_Down => letter('B'),
        gdk::Key::Right | gdk::Key::KP_Right => letter('C'),
        gdk::Key::Left | gdk::Key::KP_Left => letter('D'),
        gdk::Key::Home | gdk::Key::KP_Home => letter('H'),
        gdk::Key::End | gdk::Key::KP_End => letter('F'),
        gdk::Key::Insert | gdk::Key::KP_Insert => tilde(2),
        gdk::Key::Delete | gdk::Key::KP_Delete => tilde(3),
        gdk::Key::Page_Up | gdk::Key::KP_Page_Up => tilde(5),
        gdk::Key::Page_Down | gdk::Key::KP_Page_Down => tilde(6),
        gdk::Key::F1 => ss3('P'),
        gdk::Key::F2 => ss3('Q'),
        gdk::Key::F3 => ss3('R'),
        gdk::Key::F4 => ss3('S'),
        gdk::Key::F5 => tilde(15),
        gdk::Key::F6 => tilde(17),
        gdk::Key::F7 => tilde(18),
        gdk::Key::F8 => tilde(19),
        gdk::Key::F9 => tilde(20),
        gdk::Key::F10 => tilde(21),
        gdk::Key::F11 => tilde(23),
        gdk::Key::F12 => tilde(24),
        _ => {
            let c = key.to_lower().to_unicode()?;
            if ctrl && !shift {
                let b = match c {
                    'a'..='z' => c as u8 - b'a' + 1,
                    '@' | ' ' | '2' => 0,
                    '[' | '3' => 0x1b,
                    '\\' | '4' => 0x1c,
                    ']' | '5' => 0x1d,
                    '^' | '6' => 0x1e,
                    '_' | '7' | '/' => 0x1f,
                    '?' | '8' => 0x7f,
                    _ => return None,
                };
                esc(&[b])
            } else if alt && !ctrl {
                let c = key.to_unicode()?;
                esc(c.to_string().as_bytes())
            } else {
                return None;
            }
        }
    })
}

/// Line weights of a box drawing character: left, right, up, down.
/// 1 light, 2 heavy, 3 double.
fn box_lines(c: char) -> Option<[u8; 4]> {
    Some(match c {
        '─' => [1, 1, 0, 0],
        '━' => [2, 2, 0, 0],
        '│' => [0, 0, 1, 1],
        '┃' => [0, 0, 2, 2],
        '┌' | '╭' => [0, 1, 0, 1],
        '┐' | '╮' => [1, 0, 0, 1],
        '└' | '╰' => [0, 1, 1, 0],
        '┘' | '╯' => [1, 0, 1, 0],
        '┏' => [0, 2, 0, 2],
        '┓' => [2, 0, 0, 2],
        '┗' => [0, 2, 2, 0],
        '┛' => [2, 0, 2, 0],
        '├' => [0, 1, 1, 1],
        '┤' => [1, 0, 1, 1],
        '┬' => [1, 1, 0, 1],
        '┴' => [1, 1, 1, 0],
        '┼' => [1, 1, 1, 1],
        '┣' => [0, 2, 2, 2],
        '┫' => [2, 0, 2, 2],
        '┳' => [2, 2, 0, 2],
        '┻' => [2, 2, 2, 0],
        '╋' => [2, 2, 2, 2],
        '═' => [3, 3, 0, 0],
        '║' => [0, 0, 3, 3],
        '╔' => [0, 3, 0, 3],
        '╗' => [3, 0, 0, 3],
        '╚' => [0, 3, 3, 0],
        '╝' => [3, 0, 3, 0],
        '╠' => [0, 3, 3, 3],
        '╣' => [3, 0, 3, 3],
        '╦' => [3, 3, 0, 3],
        '╩' => [3, 3, 3, 0],
        '╬' => [3, 3, 3, 3],
        '╴' => [1, 0, 0, 0],
        '╶' => [0, 1, 0, 0],
        '╵' => [0, 0, 1, 0],
        '╷' => [0, 0, 0, 1],
        _ => return None,
    })
}

/// Draws box drawing and block characters to fill the whole cell, as
/// font glyphs leave gaps between rows. False for other characters.
fn draw_special(s: &gtk::Snapshot, c: char, x: f32, y: f32, w: f32, h: f32, color: &gdk::RGBA) -> bool {
    let rect = |s: &gtk::Snapshot, x: f32, y: f32, w: f32, h: f32| s.append_color(color, &graphene::Rect::new(x, y, w, h));
    if let Some(lines) = box_lines(c) {
        let t = (w / 8.0).round().max(1.0);
        let (cx, cy) = ((x + w / 2.0 - t / 2.0).round(), (y + h / 2.0 - t / 2.0).round());
        let [l, r, u, d] = lines;
        // Strokes for one side: offsets across the line and thickness.
        let strokes = |weight: u8| -> Vec<(f32, f32)> {
            match weight {
                1 => vec![(0.0, t)],
                2 => vec![(-t / 2.0, 2.0 * t)],
                3 => vec![(-t, t), (t, t)],
                _ => vec![],
            }
        };
        for (o, th) in strokes(l) {
            rect(s, x, cy + o, cx - x + t, th);
        }
        for (o, th) in strokes(r) {
            rect(s, cx, cy + o, x + w - cx, th);
        }
        for (o, th) in strokes(u) {
            rect(s, cx + o, y, th, cy - y + t);
        }
        for (o, th) in strokes(d) {
            rect(s, cx + o, cy, th, y + h - cy);
        }
        return true;
    }
    let n = c as u32;
    match n {
        0x2588 => rect(s, x, y, w, h),
        0x2580 => rect(s, x, y, w, (h / 2.0).round()),
        // Lower eighths up to a full block.
        0x2581..=0x2587 => {
            let part = (h * (n - 0x2580) as f32 / 8.0).round();
            rect(s, x, y + h - part, w, part)
        }
        // Left eighths, from seven down to one.
        0x2589..=0x258f => rect(s, x, y, (w * (0x2590 - n) as f32 / 8.0).round(), h),
        0x2590 => {
            let half = (w / 2.0).round();
            rect(s, x + half, y, w - half, h)
        }
        0x2591..=0x2593 => {
            let mut c = *color;
            c.set_alpha(color.alpha() * (n - 0x2590) as f32 / 4.0);
            s.append_color(&c, &graphene::Rect::new(x, y, w, h));
        }
        0x2594 => rect(s, x, y, w, (h / 8.0).round().max(1.0)),
        0x2595 => {
            let e = (w / 8.0).round().max(1.0);
            rect(s, x + w - e, y, e, h)
        }
        _ => return false,
    }
    true
}

/// Finds OSC 7 (working directory) in program output, across chunks.
/// The emulator ignores it; shells send it after each command.
#[derive(Default)]
pub(crate) struct Osc7 {
    /// Bytes of the "ESC ] 7 ;" start matched so far.
    matched: usize,
    body: Option<Vec<u8>>,
    esc: bool,
}

impl Osc7 {
    /// Last complete URI in `data`, if any.
    fn scan(&mut self, data: &[u8]) -> Option<String> {
        const START: &[u8] = b"\x1b]7;";
        let mut found = None;
        for &b in data {
            if let Some(body) = self.body.as_mut() {
                let end = b == 0x07 || (self.esc && b == b'\\');
                if end {
                    found = Some(String::from_utf8_lossy(body).into_owned());
                    self.body = None;
                } else if b == 0x1b {
                    self.esc = true;
                } else if self.esc || body.len() > 4096 {
                    // Not a string terminator: not ours.
                    self.body = None;
                } else {
                    body.push(b);
                }
                continue;
            }
            if b == START[self.matched] {
                self.matched += 1;
                if self.matched == START.len() {
                    self.matched = 0;
                    self.body = Some(Vec::new());
                    self.esc = false;
                }
            } else {
                self.matched = usize::from(b == 0x1b);
            }
        }
        found
    }
}

/// Mouse report bytes. `button` holds the button code plus modifier and
/// motion bits; release in the classic form is button 3.
fn mouse_bytes(button: u8, press: bool, col: usize, row: usize, sgr: bool) -> String {
    if sgr {
        let end = if press { 'M' } else { 'm' };
        return format!("\x1b[<{button};{};{}{end}", col + 1, row + 1);
    }
    let b = if press { button } else { (button & !3) | 3 };
    // The classic form tops out at column and row 223.
    let pos = |v: usize| char::from((v.min(222) + 33) as u8);
    format!("\x1b[M{}{}{}", char::from(32 + b), pos(col), pos(row))
}


mod imp {
    use super::*;

    pub struct View {
        pub(super) term: RefCell<Term<Events>>,
        pub(super) parser: RefCell<Processor>,
        pub(super) events: Events,
        pub(super) palette: RefCell<Palette>,
        pub(super) font: RefCell<pango::FontDescription>,
        pub(super) scale: Cell<f64>,
        /// Cell width and height in pixels.
        pub(super) cell: Cell<(f64, f64)>,
        /// Baseline of the main font from the cell top.
        pub(super) ascent: Cell<f64>,
        pub(super) size: Cell<(usize, usize)>,
        pub(super) scrollback: Cell<usize>,
        pub(super) title: RefCell<String>,
        /// Working directory URI from OSC 7.
        pub(super) cwd: RefCell<Option<String>>,
        pub(super) osc7: RefCell<Osc7>,
        /// Pointer hidden while typing.
        pub(super) hidden: Cell<bool>,
        pub(super) im: gtk::IMMulticontext,
        /// Fractional scroll steps of smooth scrolling.
        pub(super) wheel: Cell<f64>,
        pub(super) vadj: RefCell<Option<gtk::Adjustment>>,
        pub(super) hadj: RefCell<Option<gtk::Adjustment>>,
        pub(super) policy: Cell<(gtk::ScrollablePolicy, gtk::ScrollablePolicy)>,
        /// Set while the adjustment follows the view, so its own change is ignored.
        pub(super) syncing: Cell<bool>,
        /// Cursor shown in the blink cycle.
        pub(super) blink_on: Cell<bool>,
        pub(super) blink: RefCell<Option<glib::SourceId>>,
        /// Last pointer position, for wheel reports.
        pub(super) pointer: Cell<(f64, f64)>,
        /// Button held while a program gets mouse reports.
        pub(super) held: Cell<Option<u8>>,
        /// Last reported cell, so motion is sent once per cell.
        pub(super) reported: Cell<(usize, usize)>,
    }

    impl Default for View {
        fn default() -> Self {
            let events = Events::default();
            let size = Size { cols: 80, rows: 24 };
            View {
                term: RefCell::new(Term::new(Config::default(), &size, events.clone())),
                parser: RefCell::new(Processor::new()),
                events,
                palette: RefCell::default(),
                font: RefCell::new(pango::FontDescription::from_string("Monospace 11")),
                scale: Cell::new(1.0),
                cell: Cell::new((8.0, 16.0)),
                ascent: Cell::new(12.0),
                size: Cell::new((80, 24)),
                scrollback: Cell::new(Config::default().scrolling_history),
                title: RefCell::default(),
                cwd: RefCell::default(),
                osc7: RefCell::default(),
                hidden: Cell::new(false),
                im: gtk::IMMulticontext::new(),
                wheel: Cell::new(0.0),
                vadj: RefCell::default(),
                hadj: RefCell::default(),
                policy: Cell::new((gtk::ScrollablePolicy::Minimum, gtk::ScrollablePolicy::Minimum)),
                syncing: Cell::new(false),
                blink_on: Cell::new(true),
                blink: RefCell::default(),
                pointer: Cell::new((0.0, 0.0)),
                held: Cell::new(None),
                reported: Cell::new((usize::MAX, usize::MAX)),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for View {
        const NAME: &'static str = "TvView";
        type Type = super::View;
        type ParentType = gtk::Widget;
        type Interfaces = (gtk::Scrollable,);

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("termview");
        }
    }

    impl ObjectImpl for View {
        // Same names and arguments as VTE's signals.
        fn signals() -> &'static [glib::subclass::Signal] {
            static SIGNALS: std::sync::OnceLock<Vec<glib::subclass::Signal>> = std::sync::OnceLock::new();
            SIGNALS.get_or_init(|| {
                use glib::subclass::Signal;
                vec![
                    Signal::builder("commit").param_types([String::static_type(), u32::static_type()]).build(),
                    Signal::builder("child-exited").param_types([i32::static_type()]).build(),
                    Signal::builder("window-title-changed").build(),
                    Signal::builder("current-directory-uri-changed").build(),
                ]
            })
        }

        // gtk::Scrollable: a scrolled window shows the scrollback as a scrollbar.
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPS: std::sync::OnceLock<Vec<glib::ParamSpec>> = std::sync::OnceLock::new();
            PROPS.get_or_init(|| {
                ["hadjustment", "vadjustment", "hscroll-policy", "vscroll-policy"]
                    .into_iter()
                    .map(glib::ParamSpecOverride::for_interface::<gtk::Scrollable>)
                    .collect()
            })
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            let (h, v) = self.policy.get();
            match pspec.name() {
                "hadjustment" => {
                    self.hadj.replace(value.get().ok().flatten());
                }
                "vadjustment" => self.obj().set_vadj(value.get().ok().flatten()),
                "hscroll-policy" => self.policy.set((value.get().unwrap_or(h), v)),
                "vscroll-policy" => self.policy.set((h, value.get().unwrap_or(v))),
                _ => {}
            }
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                "hadjustment" => self.hadj.borrow().to_value(),
                "vadjustment" => self.vadj.borrow().to_value(),
                "hscroll-policy" => self.policy.get().0.to_value(),
                _ => self.policy.get().1.to_value(),
            }
        }

        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_focusable(true);
            obj.set_focus_on_click(true);
            obj.set_hexpand(true);
            obj.set_vexpand(true);
            obj.set_cursor_from_name(Some("text"));
            obj.measure_cell();
            obj.setup_input();
        }
    }

    impl ScrollableImpl for View {}

    impl WidgetImpl for View {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let (w, h) = self.cell.get();
            // Small minimum so panes can shrink; natural size is 80x24.
            match orientation {
                gtk::Orientation::Horizontal => ((w * 2.0) as i32, (w * 80.0) as i32, -1, -1),
                _ => (h as i32, (h * 24.0) as i32, -1, -1),
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            self.obj().fit(width, height);
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.obj().draw(snapshot);
        }
    }
}

/// Widgets built on the view (the pty terminal).
pub trait ViewImpl: WidgetImpl + ObjectSubclass<Type: IsA<View>> {}

unsafe impl<T: ViewImpl> IsSubclassable<T> for View {}

glib::wrapper! {
    pub struct View(ObjectSubclass<imp::View>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Scrollable;
}

impl Default for View {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl View {
    /// Runs program output through the emulator.
    pub fn feed(&self, data: &[u8]) {
        let imp = self.imp();
        let uri = imp.osc7.borrow_mut().scan(data);
        if let Some(uri) = uri {
            if imp.cwd.borrow().as_deref() != Some(uri.as_str()) {
                imp.cwd.replace(Some(uri));
                self.emit_by_name::<()>("current-directory-uri-changed", &[]);
            }
        }
        {
            let mut term = imp.term.borrow_mut();
            imp.parser.borrow_mut().advance(&mut *term, data);
        }
        self.handle_events();
        self.sync_scroll();
        if self.has_focus() {
            self.restart_blink();
        }
        self.queue_draw();
    }

    /// Text typed or pasted, and replies to terminal queries; with its
    /// length in bytes.
    pub fn connect_commit(&self, f: impl Fn(&View, &str, u32) + 'static) -> glib::SignalHandlerId {
        self.connect_local("commit", false, move |args| {
            if let (Ok(v), Ok(text), Ok(n)) = (args[0].get::<View>(), args[1].get::<String>(), args[2].get::<u32>()) {
                f(&v, &text, n);
            }
            None
        })
    }

    /// The program ended, with its wait status.
    pub fn connect_child_exited(&self, f: impl Fn(&View, i32) + 'static) -> glib::SignalHandlerId {
        self.connect_local("child-exited", false, move |args| {
            if let (Ok(v), Ok(status)) = (args[0].get::<View>(), args[1].get::<i32>()) {
                f(&v, status);
            }
            None
        })
    }

    pub fn connect_window_title_changed(&self, f: impl Fn(&View) + 'static) -> glib::SignalHandlerId {
        self.connect_local("window-title-changed", false, move |args| {
            if let Ok(v) = args[0].get::<View>() {
                f(&v);
            }
            None
        })
    }

    pub fn connect_current_directory_uri_changed(&self, f: impl Fn(&View) + 'static) -> glib::SignalHandlerId {
        self.connect_local("current-directory-uri-changed", false, move |args| {
            if let Ok(v) = args[0].get::<View>() {
                f(&v);
            }
            None
        })
    }

    /// Working directory the shell reported (OSC 7), as a file URI.
    pub fn current_directory_uri(&self) -> Option<String> {
        self.imp().cwd.borrow().clone()
    }

    pub fn column_count(&self) -> i64 {
        self.imp().size.get().0 as i64
    }

    pub fn row_count(&self) -> i64 {
        self.imp().size.get().1 as i64
    }

    pub fn window_title(&self) -> Option<String> {
        let t = self.imp().title.borrow();
        (!t.is_empty()).then(|| t.clone())
    }

    pub fn set_font(&self, font: Option<&pango::FontDescription>) {
        self.imp().font.replace(font.cloned().unwrap_or_else(|| pango::FontDescription::from_string("Monospace 11")));
        self.measure_cell();
    }

    pub fn font_scale(&self) -> f64 {
        self.imp().scale.get()
    }

    pub fn set_font_scale(&self, scale: f64) {
        self.imp().scale.set(scale.clamp(0.25, 4.0));
        self.measure_cell();
    }

    pub fn set_colors(&self, fg: Option<&gdk::RGBA>, bg: Option<&gdk::RGBA>, palette: &[&gdk::RGBA]) {
        let mut p = self.imp().palette.borrow_mut();
        let base = Palette::default();
        p.fg = fg.copied().unwrap_or(base.fg);
        p.bg = bg.copied().unwrap_or(base.bg);
        for (i, c) in p.colors.iter_mut().enumerate() {
            *c = palette.get(i).map(|c| **c).unwrap_or(base.colors[i]);
        }
        drop(p);
        self.queue_draw();
    }

    pub fn set_color_cursor(&self, c: Option<&gdk::RGBA>) {
        self.imp().palette.borrow_mut().cursor = c.copied();
        self.queue_draw();
    }

    pub fn set_color_highlight(&self, c: Option<&gdk::RGBA>) {
        self.imp().palette.borrow_mut().highlight = c.copied();
        self.queue_draw();
    }

    pub fn set_scrollback_lines(&self, lines: i64) {
        let n = lines.clamp(0, 1_000_000) as usize;
        self.imp().scrollback.set(n);
        self.imp().term.borrow_mut().set_options(Config { scrolling_history: n, ..Config::default() });
    }

    /// Clears the screen state; `history` also drops the scrollback.
    pub fn reset(&self, _clear_tabs: bool, history: bool) {
        let imp = self.imp();
        let (cols, rows) = imp.size.get();
        if history {
            let cfg = Config { scrolling_history: imp.scrollback.get(), ..Config::default() };
            imp.term.replace(Term::new(cfg, &Size { cols, rows }, imp.events.clone()));
        } else {
            imp.parser.borrow_mut().advance(&mut *imp.term.borrow_mut(), b"\x1bc");
        }
        imp.parser.replace(Processor::new());
        self.sync_scroll();
        self.queue_draw();
    }

    /// Whole text: scrollback and screen.
    pub fn text(&self) -> String {
        let term = self.imp().term.borrow();
        let end = Point::new(term.bottommost_line(), term.last_column());
        term.bounds_to_string(Point::new(term.topmost_line(), Column(0)), end)
    }

    pub fn has_selection(&self) -> bool {
        self.imp().term.borrow().selection.as_ref().is_some_and(|s| !s.is_empty())
    }

    pub fn copy_clipboard(&self) {
        if let Some(text) = self.imp().term.borrow().selection_to_string() {
            self.clipboard().set_text(&text);
        }
    }

    pub fn paste_clipboard(&self) {
        let v = self.clone();
        self.clipboard().read_text_async(None::<&gtk::gio::Cancellable>, move |r| {
            if let Ok(Some(text)) = r {
                v.paste(&text);
            }
        });
    }

    fn paste(&self, text: &str) {
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        if self.imp().term.borrow().mode().contains(TermMode::BRACKETED_PASTE) {
            // A paste must not end bracketed mode early.
            let text = text.replace("\x1b[201~", "");
            self.send(&format!("\x1b[200~{text}\x1b[201~"));
        } else {
            self.send(&text);
        }
    }

    /// User input: back to the bottom, then to the commit handlers.
    fn send(&self, text: &str) {
        let imp = self.imp();
        self.restart_blink();
        if !imp.hidden.replace(true) {
            self.set_cursor_from_name(Some("none"));
        }
        {
            let mut term = imp.term.borrow_mut();
            if term.grid().display_offset() != 0 {
                term.scroll_display(Scroll::Bottom);
                drop(term);
                self.sync_scroll();
                self.queue_draw();
            }
        }
        self.emit_commit(text);
    }

    fn emit_commit(&self, text: &str) {
        let n = u32::try_from(text.len()).unwrap_or(u32::MAX);
        self.emit_by_name::<()>("commit", &[&text, &n]);
    }

    fn handle_events(&self) {
        let imp = self.imp();
        let events: Vec<Event> = std::mem::take(&mut *imp.events.0.borrow_mut());
        for e in events {
            match e {
                Event::PtyWrite(s) => self.emit_commit(&s),
                Event::Title(t) => self.set_title(t),
                Event::ResetTitle => self.set_title(String::new()),
                Event::ClipboardStore(_, s) => self.clipboard().set_text(&s),
                Event::ColorRequest(i, f) => {
                    let c = {
                        let term = imp.term.borrow();
                        imp.palette.borrow().indexed(i, term.colors())
                    };
                    let px = |v: f32| (v * 255.0).round() as u8;
                    let reply = f(Rgb { r: px(c.red()), g: px(c.green()), b: px(c.blue()) });
                    self.emit_commit(&reply);
                }
                Event::TextAreaSizeRequest(f) => {
                    let (cols, rows) = imp.size.get();
                    let (w, h) = imp.cell.get();
                    let ws = WindowSize { num_lines: rows as u16, num_cols: cols as u16, cell_width: w as u16, cell_height: h as u16 };
                    self.emit_commit(&f(ws));
                }
                _ => {}
            }
        }
    }

    fn set_title(&self, t: String) {
        if *self.imp().title.borrow() == t {
            return;
        }
        self.imp().title.replace(t);
        self.emit_by_name::<()>("window-title-changed", &[]);
    }

    fn scaled_font(&self) -> pango::FontDescription {
        let mut f = self.imp().font.borrow().clone();
        let size = if f.size() > 0 { f.size() } else { 11 * pango::SCALE };
        let s = (f64::from(size) * self.imp().scale.get()).round() as i32;
        if f.is_size_absolute() {
            f.set_absolute_size(f64::from(s));
        } else {
            f.set_size(s);
        }
        f
    }

    /// Cell size from the font; refits the grid.
    fn measure_cell(&self) {
        let layout = self.create_pango_layout(Some("MMMMMMMMMM"));
        layout.set_font_description(Some(&self.scaled_font()));
        let (_, logical) = layout.extents();
        let w = f64::from(logical.width()) / f64::from(pango::SCALE) / 10.0;
        let h = f64::from(logical.height()) / f64::from(pango::SCALE);
        self.imp().cell.set((w.max(1.0), h.ceil().max(1.0)));
        self.imp().ascent.set(f64::from(layout.baseline()) / f64::from(pango::SCALE));
        self.queue_resize();
        if self.width() > 0 {
            self.fit(self.width(), self.height());
        }
    }

    fn fit(&self, width: i32, height: i32) {
        let imp = self.imp();
        let (w, h) = imp.cell.get();
        let cols = ((f64::from(width) / w) as usize).max(2);
        let rows = ((f64::from(height) / h) as usize).max(1);
        if imp.size.replace((cols, rows)) == (cols, rows) {
            return;
        }
        imp.term.borrow_mut().resize(Size { cols, rows });
        self.sync_scroll();
        self.queue_draw();
    }

    /// Grid point and cell side under widget coordinates.
    fn point_at(&self, x: f64, y: f64) -> (Point, Direction) {
        let imp = self.imp();
        let (w, h) = imp.cell.get();
        let (cols, rows) = imp.size.get();
        let col = ((x / w).max(0.0) as usize).min(cols - 1);
        let row = ((y / h).max(0.0) as usize).min(rows - 1);
        let side = if x - col as f64 * w > w / 2.0 { Direction::Right } else { Direction::Left };
        let offset = imp.term.borrow().grid().display_offset() as i32;
        (Point::new(Line(row as i32 - offset), Column(col)), side)
    }

    fn setup_input(&self) {
        let imp = self.imp();
        imp.im.set_client_widget(Some(self));
        let v = self.downgrade();
        imp.im.connect_commit(move |_, text| {
            if let Some(v) = v.upgrade() {
                v.send(text);
            }
        });

        let keys = gtk::EventControllerKey::new();
        keys.set_im_context(Some(&imp.im));
        let v = self.downgrade();
        keys.connect_key_pressed(move |_, key, _, m| {
            let Some(v) = v.upgrade() else { return glib::Propagation::Proceed };
            let shift = m.contains(gdk::ModifierType::SHIFT_MASK);
            let ctrl = m.contains(gdk::ModifierType::CONTROL_MASK);
            // Ctrl+Shift is for the application's shortcuts.
            if ctrl && shift {
                return glib::Propagation::Proceed;
            }
            if shift && matches!(key, gdk::Key::Page_Up | gdk::Key::Page_Down) {
                let s = if key == gdk::Key::Page_Up { Scroll::PageUp } else { Scroll::PageDown };
                v.imp().term.borrow_mut().scroll_display(s);
                v.sync_scroll();
                v.queue_draw();
                return glib::Propagation::Stop;
            }
            if shift && key == gdk::Key::Insert {
                v.paste_clipboard();
                return glib::Propagation::Stop;
            }
            let mode = *v.imp().term.borrow().mode();
            match key_bytes(key, m, mode) {
                Some(b) => {
                    v.send(&String::from_utf8_lossy(&b));
                    glib::Propagation::Stop
                }
                None => glib::Propagation::Proceed,
            }
        });
        self.add_controller(keys);

        let focus = gtk::EventControllerFocus::new();
        let v = self.downgrade();
        focus.connect_enter(move |_| {
            if let Some(v) = v.upgrade() {
                v.imp().im.focus_in();
                v.focus_report(true);
                v.restart_blink();
                v.queue_draw();
            }
        });
        let v = self.downgrade();
        focus.connect_leave(move |_| {
            if let Some(v) = v.upgrade() {
                v.imp().im.focus_out();
                v.focus_report(false);
                if let Some(id) = v.imp().blink.take() {
                    id.remove();
                }
                v.imp().blink_on.set(true);
                v.queue_draw();
            }
        });
        self.add_controller(focus);

        // Programs that ask for the mouse get it; Shift keeps it for selecting.
        let report = gtk::GestureClick::new();
        report.set_button(0);
        report.set_propagation_phase(gtk::PropagationPhase::Capture);
        let v = self.downgrade();
        report.connect_pressed(move |g, _, x, y| {
            let Some(v) = v.upgrade() else { return };
            let m = g.current_event_state();
            if !v.reporting(m) {
                return;
            }
            v.grab_focus();
            let b = match g.current_button() {
                gdk::BUTTON_PRIMARY => 0,
                gdk::BUTTON_MIDDLE => 1,
                gdk::BUTTON_SECONDARY => 2,
                _ => return,
            };
            v.imp().held.set(Some(b));
            v.mouse_report(b, true, x, y, m);
            g.set_state(gtk::EventSequenceState::Claimed);
        });
        let v = self.downgrade();
        report.connect_released(move |g, _, x, y| {
            let Some(v) = v.upgrade() else { return };
            if let Some(b) = v.imp().held.take() {
                v.mouse_report(b, false, x, y, g.current_event_state());
            }
        });
        self.add_controller(report);

        let motion = gtk::EventControllerMotion::new();
        let v = self.downgrade();
        motion.connect_motion(move |c, x, y| {
            let Some(v) = v.upgrade() else { return };
            if v.imp().hidden.replace(false) {
                v.set_cursor_from_name(Some("text"));
            }
            v.imp().pointer.set((x, y));
            let mode = *v.imp().term.borrow().mode();
            let held = v.imp().held.get();
            let wanted = mode.contains(TermMode::MOUSE_MOTION) || (held.is_some() && mode.contains(TermMode::MOUSE_DRAG));
            if !wanted || !v.reporting(c.current_event_state()) {
                return;
            }
            let cell = v.cell_at(x, y);
            if v.imp().reported.replace(cell) == cell {
                return;
            }
            // Motion adds 32; with no button held the button is 3.
            v.mouse_report(32 + held.unwrap_or(3), true, x, y, c.current_event_state());
        });
        self.add_controller(motion);

        // Press starts a selection (double: word, triple: line); drag extends it.
        let click = gtk::GestureClick::new();
        click.set_button(gdk::BUTTON_PRIMARY);
        let v = self.downgrade();
        click.connect_pressed(move |g, n, x, y| {
            let Some(v) = v.upgrade() else { return };
            v.grab_focus();
            let (p, side) = v.point_at(x, y);
            let ty = match n {
                2 => SelectionType::Semantic,
                3.. => SelectionType::Lines,
                _ if g.current_event_state().contains(gdk::ModifierType::ALT_MASK) => SelectionType::Block,
                _ => SelectionType::Simple,
            };
            v.imp().term.borrow_mut().selection = Some(Selection::new(ty, p, side));
            v.queue_draw();
        });
        let v = self.downgrade();
        click.connect_released(move |_, _, _, _| {
            if let Some(v) = v.upgrade() {
                if let Some(text) = v.imp().term.borrow().selection_to_string() {
                    v.primary_clipboard().set_text(&text);
                }
            }
        });
        self.add_controller(click);

        let drag = gtk::GestureDrag::new();
        drag.set_button(gdk::BUTTON_PRIMARY);
        let v = self.downgrade();
        drag.connect_drag_update(move |g, dx, dy| {
            let Some(v) = v.upgrade() else { return };
            let Some((x0, y0)) = g.start_point() else { return };
            let (p, side) = v.point_at(x0 + dx, y0 + dy);
            if let Some(s) = v.imp().term.borrow_mut().selection.as_mut() {
                s.update(p, side);
            }
            v.queue_draw();
        });
        let v = self.downgrade();
        drag.connect_drag_end(move |_, _, _| {
            if let Some(v) = v.upgrade() {
                if let Some(text) = v.imp().term.borrow().selection_to_string() {
                    v.primary_clipboard().set_text(&text);
                }
            }
        });
        self.add_controller(drag);

        // Middle click pastes the primary selection.
        let middle = gtk::GestureClick::new();
        middle.set_button(gdk::BUTTON_MIDDLE);
        let v = self.downgrade();
        middle.connect_pressed(move |_, _, _, _| {
            let Some(v) = v.upgrade() else { return };
            let w = v.clone();
            v.primary_clipboard().read_text_async(None::<&gtk::gio::Cancellable>, move |r| {
                if let Ok(Some(text)) = r {
                    w.paste(&text);
                }
            });
        });
        self.add_controller(middle);

        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let v = self.downgrade();
        scroll.connect_scroll(move |c, _, dy| {
            let Some(v) = v.upgrade() else { return glib::Propagation::Proceed };
            v.wheel(dy, c.current_event_state());
            glib::Propagation::Stop
        });
        self.add_controller(scroll);
    }

    /// Shows the cursor and starts its blink cycle again, as the desktop
    /// settings say.
    fn restart_blink(&self) {
        let imp = self.imp();
        imp.blink_on.set(true);
        if let Some(id) = imp.blink.take() {
            id.remove();
        }
        let settings = self.settings();
        if !settings.is_gtk_cursor_blink() {
            return;
        }
        let half = u64::try_from(settings.gtk_cursor_blink_time() / 2).unwrap_or(600).max(100);
        let v = self.downgrade();
        let id = glib::timeout_add_local(std::time::Duration::from_millis(half), move || {
            let Some(v) = v.upgrade() else { return glib::ControlFlow::Break };
            v.imp().blink_on.set(!v.imp().blink_on.get());
            v.queue_draw();
            glib::ControlFlow::Continue
        });
        imp.blink.replace(Some(id));
    }

    fn set_vadj(&self, adj: Option<gtk::Adjustment>) {
        if let Some(a) = &adj {
            let v = self.downgrade();
            a.connect_value_changed(move |a| {
                let Some(v) = v.upgrade() else { return };
                if v.imp().syncing.get() {
                    return;
                }
                // Value counts lines from the top of the scrollback.
                let want = (a.upper() - a.page_size() - a.value()).round().max(0.0) as i32;
                let now = v.imp().term.borrow().grid().display_offset() as i32;
                if want != now {
                    v.imp().term.borrow_mut().scroll_display(Scroll::Delta(want - now));
                    v.queue_draw();
                }
            });
        }
        self.imp().vadj.replace(adj);
        self.sync_scroll();
    }

    /// Puts the scrollbar where the view is: one step per line.
    fn sync_scroll(&self) {
        let imp = self.imp();
        let Some(a) = imp.vadj.borrow().clone() else { return };
        let (history, offset) = {
            let term = imp.term.borrow();
            (term.grid().history_size(), term.grid().display_offset())
        };
        let rows = imp.size.get().1 as f64;
        let upper = history as f64 + rows;
        imp.syncing.set(true);
        a.configure(upper - rows - offset as f64, 0.0, upper, 1.0, rows, rows);
        imp.syncing.set(false);
    }

    /// True when a program wants mouse events and Shift is not held.
    fn reporting(&self, m: gdk::ModifierType) -> bool {
        self.imp().term.borrow().mode().intersects(TermMode::MOUSE_MODE) && !m.contains(gdk::ModifierType::SHIFT_MASK)
    }

    /// Screen column and row under widget coordinates.
    fn cell_at(&self, x: f64, y: f64) -> (usize, usize) {
        let (w, h) = self.imp().cell.get();
        let (cols, rows) = self.imp().size.get();
        (((x / w).max(0.0) as usize).min(cols - 1), ((y / h).max(0.0) as usize).min(rows - 1))
    }

    /// Sends a mouse event as the program asked: SGR or the classic form.
    fn mouse_report(&self, button: u8, press: bool, x: f64, y: f64, m: gdk::ModifierType) {
        let (col, row) = self.cell_at(x, y);
        self.imp().reported.set((col, row));
        let mods = 4 * u8::from(m.contains(gdk::ModifierType::SHIFT_MASK))
            + 8 * u8::from(m.contains(gdk::ModifierType::ALT_MASK))
            + 16 * u8::from(m.contains(gdk::ModifierType::CONTROL_MASK));
        let sgr = self.imp().term.borrow().mode().contains(TermMode::SGR_MOUSE);
        self.emit_commit(&mouse_bytes(button + mods, press, col, row, sgr));
    }

    fn wheel(&self, dy: f64, m: gdk::ModifierType) {
        let imp = self.imp();
        let acc = imp.wheel.get() + dy * 3.0;
        let lines = acc.trunc() as i32;
        imp.wheel.set(acc - f64::from(lines));
        if lines == 0 {
            return;
        }
        let mode = *imp.term.borrow().mode();
        if self.reporting(m) {
            let (x, y) = imp.pointer.get();
            let b = if lines < 0 { 64 } else { 65 };
            for _ in 0..lines.unsigned_abs() {
                self.mouse_report(b, true, x, y, m);
            }
            return;
        }
        // Full-screen programs get arrow keys instead.
        if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
            let key = match (lines < 0, mode.contains(TermMode::APP_CURSOR)) {
                (true, true) => "\x1bOA",
                (true, false) => "\x1b[A",
                (false, true) => "\x1bOB",
                (false, false) => "\x1b[B",
            };
            self.emit_commit(&key.repeat(lines.unsigned_abs() as usize));
            return;
        }
        imp.term.borrow_mut().scroll_display(Scroll::Delta(-lines));
        self.sync_scroll();
        self.queue_draw();
    }

    fn focus_report(&self, on: bool) {
        if self.imp().term.borrow().mode().contains(TermMode::FOCUS_IN_OUT) {
            self.emit_commit(if on { "\x1b[I" } else { "\x1b[O" });
        }
    }

    fn draw(&self, snapshot: &gtk::Snapshot) {
        let imp = self.imp();
        let term = imp.term.borrow();
        let pal = imp.palette.borrow();
        let (cw, ch) = imp.cell.get();
        let ascent = imp.ascent.get();
        let (width, height) = (self.width() as f32, self.height() as f32);
        snapshot.append_color(&pal.bg, &graphene::Rect::new(0.0, 0.0, width, height));

        let content = term.renderable_content();
        let offset = content.display_offset as i32;
        let colors = content.colors;
        let selection = content.selection;
        let cursor = content.cursor;
        let focused = self.has_focus();

        let base = self.scaled_font();
        let mut fonts = [base.clone(), base.clone(), base.clone(), base];
        fonts[1].set_weight(pango::Weight::Bold);
        fonts[2].set_style(pango::Style::Italic);
        fonts[3].set_weight(pango::Weight::Bold);
        fonts[3].set_style(pango::Style::Italic);
        let ctx = self.pango_context();

        // Runs of cells with the same look are drawn as one layout.
        struct Run {
            row: i32,
            col: usize,
            text: String,
            fg: gdk::RGBA,
            font: usize,
            underline: bool,
            strike: bool,
        }
        let mut runs: Vec<Run> = Vec::new();
        let mut cursor_cell: Option<(i32, usize, char, gdk::RGBA, gdk::RGBA, bool)> = None;

        for cell in content.display_iter {
            let flags = cell.flags;
            if flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let row = cell.point.line.0 + offset;
            let col = cell.point.column.0;
            let mut fg = pal.color(cell.fg, colors);
            let mut bg = pal.color(cell.bg, colors);
            if flags.contains(Flags::DIM) {
                fg = dim(fg);
            }
            if flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            let picked = selection.is_some_and(|s| s.contains_cell(&cell, cursor.point, cursor.shape));
            if picked {
                match pal.highlight {
                    Some(h) => bg = h,
                    None => std::mem::swap(&mut fg, &mut bg),
                }
            }
            if flags.contains(Flags::HIDDEN) {
                fg = bg;
            }
            let wide = flags.contains(Flags::WIDE_CHAR);
            let span = if wide { 2.0 } else { 1.0 };
            if bg != pal.bg {
                let r = graphene::Rect::new((col as f64 * cw) as f32, (f64::from(row) * ch) as f32, (cw * span).ceil() as f32, ch as f32);
                snapshot.append_color(&bg, &r);
            }
            if cell.point == cursor.point && cursor.shape != CursorShape::Hidden {
                cursor_cell = Some((row, col, cell.c, fg, bg, wide));
            }
            if cell.c == ' ' || cell.c == '\t' {
                continue;
            }
            let x = (col as f64 * cw) as f32;
            let y = (f64::from(row) * ch) as f32;
            if draw_special(snapshot, cell.c, x, y, cw as f32, ch as f32, &fg) {
                continue;
            }
            let font = usize::from(flags.contains(Flags::BOLD)) + 2 * usize::from(flags.contains(Flags::ITALIC));
            let underline = flags.intersects(Flags::ALL_UNDERLINES);
            let strike = flags.contains(Flags::STRIKEOUT);
            let mut text = String::from(cell.c);
            if let Some(extra) = cell.zerowidth() {
                text.extend(extra.iter());
            }
            // Wide cells and combining marks start their own run so the grid stays aligned.
            match runs.last_mut() {
                Some(r) if !wide && r.row == row && r.col + r.text.chars().count() == col && r.fg == fg && r.font == font && r.underline == underline && r.strike == strike && cell.zerowidth().is_none() && r.text.chars().all(|c| c.len_utf8() < 3) && cell.c.len_utf8() < 3 => {
                    r.text.push(cell.c);
                }
                _ => runs.push(Run { row, col, text, fg, font, underline, strike }),
            }
        }

        for r in &runs {
            let layout = pango::Layout::new(&ctx);
            layout.set_font_description(Some(&fonts[r.font]));
            layout.set_text(&r.text);
            if r.underline || r.strike {
                let attrs = pango::AttrList::new();
                if r.underline {
                    attrs.insert(pango::AttrInt::new_underline(pango::Underline::Single));
                }
                if r.strike {
                    attrs.insert(pango::AttrInt::new_strikethrough(true));
                }
                layout.set_attributes(Some(&attrs));
            }
            snapshot.save();
            // Fallback fonts have their own ascent; keep one baseline.
            let lift = ascent - f64::from(layout.baseline()) / f64::from(pango::SCALE);
            snapshot.translate(&graphene::Point::new((r.col as f64 * cw) as f32, (f64::from(r.row) * ch + lift) as f32));
            snapshot.append_layout(&layout, &r.fg);
            snapshot.restore();
        }

        if let Some((row, col, c, fg, bg, wide)) = cursor_cell {
            let color = pal.cursor.unwrap_or(fg);
            let x = (col as f64 * cw) as f32;
            let y = (f64::from(row) * ch) as f32;
            let w = (cw * if wide { 2.0 } else { 1.0 }) as f32;
            let full = graphene::Rect::new(x, y, w, ch as f32);
            let shown = imp.blink_on.get() || !focused;
            match (focused, cursor.shape) {
                _ if !shown => {}
                (true, CursorShape::Beam) => snapshot.append_color(&color, &graphene::Rect::new(x, y, 2.0, ch as f32)),
                (true, CursorShape::Underline) => snapshot.append_color(&color, &graphene::Rect::new(x, y + ch as f32 - 2.0, w, 2.0)),
                (true, _) => {
                    snapshot.append_color(&color, &full);
                    if c != ' ' && !draw_special(snapshot, c, x, y, w, ch as f32, &bg) {
                        let layout = pango::Layout::new(&ctx);
                        layout.set_font_description(Some(&fonts[0]));
                        layout.set_text(&c.to_string());
                        snapshot.save();
                        let lift = ascent - f64::from(layout.baseline()) / f64::from(pango::SCALE);
                        snapshot.translate(&graphene::Point::new(x, y + lift as f32));
                        snapshot.append_layout(&layout, &bg);
                        snapshot.restore();
                    }
                }
                (false, _) => {
                    let b = gtk::gsk::RoundedRect::from_rect(full, 0.0);
                    snapshot.append_border(&b, &[1.0; 4], &[color; 4]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        let none = gdk::ModifierType::empty();
        let ctrl = gdk::ModifierType::CONTROL_MASK;
        let mode = TermMode::default();
        assert_eq!(key_bytes(gdk::Key::Up, none, mode).as_deref(), Some(&b"\x1b[A"[..]));
        assert_eq!(key_bytes(gdk::Key::Up, none, TermMode::APP_CURSOR).as_deref(), Some(&b"\x1bOA"[..]));
        assert_eq!(key_bytes(gdk::Key::Right, ctrl, mode).as_deref(), Some(&b"\x1b[1;5C"[..]));
        assert_eq!(key_bytes(gdk::Key::c, ctrl, mode).as_deref(), Some(&b"\x03"[..]));
        assert_eq!(key_bytes(gdk::Key::F5, none, mode).as_deref(), Some(&b"\x1b[15~"[..]));
        assert_eq!(key_bytes(gdk::Key::x, gdk::ModifierType::ALT_MASK, mode).as_deref(), Some(&b"\x1bx"[..]));
        assert_eq!(key_bytes(gdk::Key::a, none, mode), None);
    }

    #[test]
    fn osc7() {
        let mut o = Osc7::default();
        assert_eq!(o.scan(b"ab\x1b]7;file://h/tmp\x07cd"), Some("file://h/tmp".into()));
        assert_eq!(o.scan(b"\x1b]7;file://h/"), None);
        assert_eq!(o.scan(b"home\x1b\\"), Some("file://h/home".into()));
        assert_eq!(o.scan(b"\x1b]"), None);
        assert_eq!(o.scan(b"7;x\x07"), Some("x".into()));
        assert_eq!(o.scan(b"\x1b]0;title\x07"), None);
    }

    #[test]
    fn mouse() {
        assert_eq!(mouse_bytes(0, true, 4, 9, true), "\x1b[<0;5;10M");
        assert_eq!(mouse_bytes(0, false, 4, 9, true), "\x1b[<0;5;10m");
        assert_eq!(mouse_bytes(64, true, 0, 0, true), "\x1b[<64;1;1M");
        assert_eq!(mouse_bytes(0, true, 0, 0, false), "\x1b[M !!");
        assert_eq!(mouse_bytes(2, false, 1, 1, false), "\x1b[M#\"\"");
    }

    #[test]
    fn colors() {
        assert_eq!(xterm(16), gdk::RGBA::new(0.0, 0.0, 0.0, 1.0));
        assert_eq!(xterm(231), gdk::RGBA::new(1.0, 1.0, 1.0, 1.0));
        let p = Palette::default();
        let set = Colors::default();
        assert_eq!(p.color(Color::Named(NamedColor::Red), &set), p.colors[1]);
        assert_eq!(p.color(Color::Named(NamedColor::Foreground), &set), p.fg);
    }
}
