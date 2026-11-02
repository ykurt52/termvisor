// Shortcut picker shared by the command button and macro editors.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib, glib::translate::IntoGlib};

/// "Ctrl+1" for "<Control>1".
pub fn label(key: &str) -> String {
    gtk::accelerator_parse(key).map(|(k, m)| gtk::accelerator_get_label(k, m).to_string()).unwrap_or_else(|| key.to_string())
}

/// Row with the current shortcut and a "Set…" button; after it, the next
/// key combination pressed in `dlg` becomes the shortcut. Returns the row
/// and the chosen shortcut (GTK accelerator form, e.g. "<Control>1").
pub fn row(init: Option<&str>, dlg: &adw::Dialog, toasts: &adw::ToastOverlay) -> (adw::ActionRow, Rc<RefCell<Option<String>>>) {
    let key: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(init.map(str::to_string)));
    let shortcut = adw::ActionRow::new();
    shortcut.set_title("Shortcut");
    shortcut.set_subtitle("Works while a terminal has the focus");
    let accel = gtk::ShortcutLabel::new(init.unwrap_or(""));
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

    let catch = gtk::EventControllerKey::new();
    catch.set_propagation_phase(gtk::PropagationPhase::Capture);
    let (k, a, r, t) = (key.clone(), accel, record, toasts.clone());
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
    (shortcut, key)
}

/// Runs `f` for the shortcut only while a terminal of the window has the
/// focus; elsewhere (dialogs, entries) the key keeps its normal meaning.
pub fn in_terminal(f: impl Fn(&vte::Terminal) + 'static) -> gtk::CallbackAction {
    gtk::CallbackAction::new(move |w, _| {
        let t = w
            .downcast_ref::<gtk::Window>()
            .and_then(gtk::prelude::GtkWindowExt::focus)
            .and_then(|f| f.ancestor(vte::Terminal::static_type()))
            .and_then(|t| t.downcast::<vte::Terminal>().ok());
        match t {
            Some(t) => {
                f(&t);
                glib::Propagation::Stop
            }
            None => glib::Propagation::Proceed,
        }
    })
}
