// SSH_ASKPASS helper mode.
//
// ssh runs `<this binary> "<prompt>"` with ENV_MODE set. We answer from the
// keyring or show a small dialog, print the answer on stdout and exit.
// Exit status 1 means "cancelled / no".

use std::cell::RefCell;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::keyring;

/// Marks the ssh environment so the binary starts in askpass mode.
pub const ENV_MODE: &str = "TERMVISOR_ASKPASS";
/// Session target, used to find the stored password.
pub const ENV_TARGET: &str = "TERMVISOR_TARGET";
/// Set when the session must never use the keyring.
pub const ENV_NO_STORE: &str = "TERMVISOR_NO_STORE";

#[derive(Debug, PartialEq)]
enum Ask {
    /// Unknown host key: answer "yes" to accept.
    HostKey,
    /// Yes/no question (SSH_ASKPASS_PROMPT=confirm).
    Confirm,
    /// Message only (SSH_ASKPASS_PROMPT=none), e.g. "touch your key".
    Notice,
    /// Password or passphrase with a keyring id.
    Secret(String),
    /// Any other text prompt, e.g. a one-time code.
    Plain,
}

fn classify(prompt: &str, mode: Option<&str>, target: Option<&str>) -> Ask {
    let p = prompt.to_lowercase();
    if p.contains("continue connecting") {
        return Ask::HostKey;
    }
    match mode {
        Some("confirm") => return Ask::Confirm,
        Some("none") => return Ask::Notice,
        _ => {}
    }
    if let Some(path) = key_path(prompt) {
        return Ask::Secret(keyring::key_id(&path));
    }
    match target {
        Some(t) if p.contains("password") => Ask::Secret(keyring::password_id(t)),
        _ => Ask::Plain,
    }
}

/// Key path from "Enter passphrase for key '/path':".
fn key_path(prompt: &str) -> Option<String> {
    let rest = prompt.split_once("passphrase for key '")?.1;
    let (path, _) = rest.split_once('\'')?;
    (!path.is_empty()).then(|| path.to_string())
}

fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3))
}

/// Set after the keyring answered once for this ssh process. A second
/// prompt means the stored secret was wrong, so we ask the user instead.
fn marker(id: &str) -> PathBuf {
    let ppid = crate::sys::parent_id();
    glib::user_runtime_dir().join("termvisor").join(format!("askpass-{ppid}-{:016x}", fnv(id)))
}

fn answer(text: &str) -> glib::ExitCode {
    println!("{text}");
    glib::ExitCode::SUCCESS
}

pub fn run(prompt: &str) -> glib::ExitCode {
    let mode = std::env::var(ENV_MODE_PROMPT).ok();
    let target = std::env::var(ENV_TARGET).ok().filter(|t| !t.is_empty());
    let no_store = std::env::var_os(ENV_NO_STORE).is_some();
    let ask = classify(prompt, mode.as_deref(), target.as_deref());

    if let Ask::Secret(id) = &ask {
        let mark = marker(id);
        if !no_store && !mark.exists() {
            if let Some(s) = keyring::get(id) {
                if let Some(dir) = mark.parent() {
                    let _ = fs::create_dir_all(dir);
                }
                if let Err(e) = fs::write(&mark, b"") {
                    crate::warn(&format!("askpass marker: {e}"));
                }
                return answer(&s);
            }
        }
    }

    let offer = matches!(ask, Ask::Secret(_)) && !no_store;
    match (dialog(&ask, prompt, offer), &ask) {
        (Some(_), Ask::HostKey) => answer("yes"),
        (Some(_), Ask::Confirm | Ask::Notice) => glib::ExitCode::SUCCESS,
        (Some((text, remember)), Ask::Secret(id)) => {
            if remember {
                if let Err(e) = keyring::set(id, &text) {
                    crate::warn(&e);
                }
            }
            answer(&text)
        }
        (Some((text, _)), Ask::Plain) => answer(&text),
        (None, _) => glib::ExitCode::FAILURE,
    }
}

/// ssh sets this for confirm/none prompts.
const ENV_MODE_PROMPT: &str = "SSH_ASKPASS_PROMPT";

/// Shows one dialog. Returns (text, remember) on OK, None on cancel.
fn dialog(ask: &Ask, prompt: &str, offer_store: bool) -> Option<(String, bool)> {
    let app = adw::Application::builder()
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let result: Rc<RefCell<Option<(String, bool)>>> = Rc::default();

    let res = result.clone();
    let prompt = prompt.trim().to_string();
    let host_key = *ask == Ask::HostKey;
    let needs_text = matches!(ask, Ask::Secret(_) | Ask::Plain);
    let notice = *ask == Ask::Notice;
    let secret = matches!(ask, Ask::Secret(_));

    app.connect_activate(move |app| {
        let label = gtk::Label::new(Some(&prompt));
        label.set_wrap(true);
        label.set_selectable(true);
        label.set_xalign(0.0);
        if host_key {
            label.add_css_class("monospace");
        }

        let entry = gtk::PasswordEntry::new();
        entry.set_show_peek_icon(true);
        entry.set_visible(needs_text);
        let remember = gtk::CheckButton::with_label("Remember in keyring");
        remember.set_visible(offer_store);
        remember.set_active(true);

        let (no_label, yes_label) = match (host_key, notice, secret) {
            (true, _, _) => ("Reject", "Accept"),
            (_, true, _) => ("Cancel", "OK"),
            (_, _, true) => ("Cancel", "Connect"),
            _ => ("No", "Yes"),
        };
        let no = gtk::Button::with_label(no_label);
        let yes = gtk::Button::with_label(yes_label);
        yes.add_css_class("suggested-action");
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        buttons.set_halign(gtk::Align::End);
        buttons.append(&no);
        buttons.append(&yes);

        let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
        body.set_margin_top(12);
        body.set_margin_bottom(12);
        body.set_margin_start(12);
        body.set_margin_end(12);
        if host_key {
            let warn = gtk::Label::new(Some("Unknown host key. Check the fingerprint before accepting."));
            warn.add_css_class("heading");
            warn.set_xalign(0.0);
            body.append(&warn);
        }
        body.append(&label);
        body.append(&entry);
        body.append(&remember);
        body.append(&buttons);

        let tv = adw::ToolbarView::new();
        tv.add_top_bar(&adw::HeaderBar::new());
        tv.set_content(Some(&body));

        let win = adw::ApplicationWindow::builder()
            .application(app)
            .title("termvisor: SSH")
            .default_width(460)
            .content(&tv)
            .build();

        let w = win.clone();
        no.connect_clicked(move |_| w.close());
        let ok = {
            let (w, res, entry, remember) = (win.clone(), res.clone(), entry.clone(), remember.clone());
            move || {
                res.replace(Some((entry.text().to_string(), remember.is_active())));
                w.close();
            }
        };
        let o = ok.clone();
        yes.connect_clicked(move |_| o());
        entry.connect_activate(move |_| ok());

        win.present();
        if needs_text {
            entry.grab_focus();
        } else {
            no.grab_focus();
        }
    });

    app.run_with_args::<&str>(&[]);
    result.take()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_prompts() {
        let hk = "The authenticity of host 'b (1.2.3.4)' can't be established.\nED25519 key fingerprint is SHA256:x.\nAre you sure you want to continue connecting (yes/no/[fingerprint])? ";
        assert_eq!(classify(hk, None, Some("b")), Ask::HostKey);
        assert_eq!(
            classify("root@b's password: ", None, Some("root@b")),
            Ask::Secret("password:root@b".into())
        );
        assert_eq!(
            classify("Enter passphrase for key '/home/u/.ssh/id_ed25519': ", None, None),
            Ask::Secret("key:/home/u/.ssh/id_ed25519".into())
        );
        assert_eq!(classify("Allow use of key?", Some("confirm"), None), Ask::Confirm);
        assert_eq!(classify("Confirm user presence", Some("none"), None), Ask::Notice);
        assert_eq!(classify("Verification code: ", None, Some("b")), Ask::Plain);
        assert_eq!(classify("root@b's password: ", None, None), Ask::Plain);
    }
}
