// Theme editor: every color change shows at once on the whole window;
// Cancel or closing puts the previous theme back.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::gdk;

use crate::state::{theme_dir, State};
use crate::theme::{self, Kind, TermFile, ThemeFile, UiFile};

/// "#rrggbb", or "rgba(…)" when not opaque.
fn hex(c: &gdk::RGBA) -> String {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    if c.alpha() < 0.999 {
        return c.to_string();
    }
    format!("#{:02x}{:02x}{:02x}", b(c.red()), b(c.green()), b(c.blue()))
}

fn button(color: &str) -> gtk::ColorDialogButton {
    let d = gtk::ColorDialog::new();
    d.set_with_alpha(false);
    let b = gtk::ColorDialogButton::new(Some(d));
    b.set_rgba(&gdk::RGBA::parse(color).unwrap_or(gdk::RGBA::BLACK));
    b.set_valign(gtk::Align::Center);
    b
}

fn color_row(group: &adw::PreferencesGroup, title: &str, color: &str) -> gtk::ColorDialogButton {
    let r = adw::ActionRow::new();
    r.set_title(title);
    let b = button(color);
    r.add_suffix(&b);
    group.add(&r);
    b
}

const PALETTE: [&str; 16] = [
    "Black", "Red", "Green", "Yellow", "Blue", "Magenta", "Cyan", "White", "Bright Black", "Bright Red", "Bright Green", "Bright Yellow",
    "Bright Blue", "Bright Magenta", "Bright Cyan", "Bright White",
];

struct Form {
    name: adw::EntryRow,
    kind: adw::ComboRow,
    ui: [gtk::ColorDialogButton; 6],
    term: [gtk::ColorDialogButton; 4],
    palette: Vec<gtk::ColorDialogButton>,
}

impl Form {
    fn file(&self) -> ThemeFile {
        let c = |b: &gtk::ColorDialogButton| hex(&b.rgba());
        ThemeFile {
            name: self.name.text().trim().to_string(),
            kind: if self.kind.selected() == 1 { Kind::Light } else { Kind::Dark },
            ui: UiFile {
                bg: c(&self.ui[0]),
                fg: c(&self.ui[1]),
                panel: c(&self.ui[2]),
                header: c(&self.ui[3]),
                accent: c(&self.ui[4]),
                border: c(&self.ui[5]),
            },
            term: TermFile {
                fg: c(&self.term[0]),
                bg: c(&self.term[1]),
                cursor: c(&self.term[2]),
                selection: c(&self.term[3]),
                palette: self.palette.iter().map(c).collect(),
            },
        }
    }
}

pub fn open(parent: &impl IsA<gtk::Widget>, state: &Rc<State>) {
    let Some(cur) = state.theme() else { return };
    // Bundled themes are copied; user themes are edited in place.
    let (mut init, old) = (cur.file.clone(), cur.path.clone());
    if old.is_none() {
        init.name = format!("{} Copy", init.name);
    }

    let general = adw::PreferencesGroup::new();
    let name = adw::EntryRow::new();
    name.set_title("Name");
    name.set_text(&init.name);
    let kind = adw::ComboRow::new();
    kind.set_title("Style");
    kind.set_model(Some(&gtk::StringList::new(&["Dark", "Light"])));
    kind.set_selected(u32::from(init.kind == Kind::Light));
    general.add(&name);
    general.add(&kind);

    let ui_group = adw::PreferencesGroup::new();
    ui_group.set_title("Window");
    let u = &init.ui;
    let ui = [
        color_row(&ui_group, "Background", &u.bg),
        color_row(&ui_group, "Text", &u.fg),
        color_row(&ui_group, "Side Panel and Dock", &u.panel),
        color_row(&ui_group, "Header and Tab Bar", &u.header),
        color_row(&ui_group, "Accent", &u.accent),
        color_row(&ui_group, "Borders", &u.border),
    ];
    let term_group = adw::PreferencesGroup::new();
    term_group.set_title("Terminal");
    let t = &init.term;
    let term = [
        color_row(&term_group, "Text", &t.fg),
        color_row(&term_group, "Background", &t.bg),
        color_row(&term_group, "Cursor", &t.cursor),
        color_row(&term_group, "Selection", &t.selection),
    ];
    let pal_group = adw::PreferencesGroup::new();
    pal_group.set_title("Palette");
    pal_group.set_description(Some("The 16 terminal colors. Bright red, green, yellow, blue, magenta and cyan also tint the window."));
    let grid = gtk::Grid::new();
    grid.set_row_spacing(6);
    grid.set_column_spacing(6);
    grid.set_halign(gtk::Align::Center);
    let mut palette = Vec::new();
    for (i, label) in PALETTE.iter().enumerate() {
        let b = button(t.palette.get(i).map(String::as_str).unwrap_or("#000000"));
        b.set_tooltip_text(Some(&format!("{i}: {label}")));
        grid.attach(&b, (i % 8) as i32, (i / 8) as i32, 1, 1);
        palette.push(b);
    }
    pal_group.add(&grid);

    let form = Rc::new(Form { name, kind, ui, term, palette });
    let page = adw::PreferencesPage::new();
    for g in [&general, &ui_group, &term_group, &pal_group] {
        page.add(g);
    }
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&page));

    let cancel = gtk::Button::with_label("Cancel");
    let save = gtk::Button::with_label("Save");
    save.add_css_class("suggested-action");
    let delete = gtk::Button::from_icon_name("user-trash-symbolic");
    delete.set_tooltip_text(Some("Delete this theme"));
    delete.set_visible(old.is_some());
    let header = adw::HeaderBar::new();
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    header.pack_start(&cancel);
    header.pack_end(&save);
    header.pack_end(&delete);
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&toasts));
    let dlg = adw::Dialog::new();
    dlg.set_title(if old.is_some() { "Edit Theme" } else { "New Theme" });
    dlg.set_content_width(520);
    dlg.set_content_height(720);
    dlg.set_child(Some(&view));

    // Live preview of every change.
    let show = {
        let (form, st) = (form.clone(), state.clone());
        move || {
            if let Ok(t) = theme::build(form.file()) {
                st.preview(Some(t));
            }
        }
    };
    show();
    let buttons: Vec<gtk::ColorDialogButton> = form.ui.iter().chain(form.term.iter()).chain(form.palette.iter()).cloned().collect();
    for b in buttons {
        let s = show.clone();
        b.connect_rgba_notify(move |_| s());
    }
    let s = show.clone();
    form.kind.connect_selected_notify(move |_| s());

    // However the dialog closes, the preview ends; Save sets the theme first.
    let st = state.clone();
    dlg.connect_closed(move |_| st.preview(None));
    let d = dlg.clone();
    cancel.connect_clicked(move |_| {
        d.close();
    });

    let old = Rc::new(RefCell::new(old));
    let (d, st, f, o, t) = (dlg.clone(), state.clone(), form.clone(), old.clone(), toasts.clone());
    save.connect_clicked(move |_| {
        let file = f.file();
        if file.name.is_empty() {
            return t.add_toast(adw::Toast::new("Give the theme a name"));
        }
        let clash = st.themes().iter().any(|x| x.name == file.name && x.path.is_none());
        if clash {
            return t.add_toast(adw::Toast::new("A bundled theme has this name"));
        }
        match theme::save(&theme_dir(), &file, o.borrow().as_deref()) {
            Ok(p) => {
                o.replace(Some(p));
                st.preview(None);
                st.reload_themes();
                st.set_theme(&file.name);
                d.close();
            }
            Err(e) => t.add_toast(adw::Toast::new(&e)),
        }
    });

    let (d, st, o, cur_name) = (dlg.clone(), state.clone(), old, cur.name.clone());
    delete.connect_clicked(move |b| {
        let Some(path) = o.borrow().clone() else { return };
        let a = adw::AlertDialog::new(Some(&format!("Delete “{cur_name}”?")), Some(&path.display().to_string()));
        a.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
        a.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        a.set_default_response(Some("cancel"));
        a.set_close_response("cancel");
        let (d, st) = (d.clone(), st.clone());
        a.connect_response(None, move |_, r| {
            if r != "delete" {
                return;
            }
            let _ = std::fs::remove_file(&path);
            st.preview(None);
            st.reload_themes();
            // The active theme is gone: fall back to the default one.
            if !st.themes().iter().any(|t| t.name == st.cfg.borrow().theme) {
                st.set_theme("Dark");
            }
            d.close();
        });
        a.present(Some(b));
    });

    dlg.present(Some(parent));
}
