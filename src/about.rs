// About window: version, publisher, links, license, release notes and the
// details worth pasting into a bug report.

use adw::prelude::*;

const METAINFO: &str = include_str!("../data/io.github.ykurt52.termvisor.metainfo.xml");

/// Body of the newest `<release>` description in the metainfo.
fn release_notes(xml: &str) -> Option<(String, String)> {
    let start = xml.find("<release ")?;
    let rest = &xml[start..];
    let version = rest.split("version=\"").nth(1)?.split('"').next()?.to_string();
    let end = rest.find("</release>")?;
    let rel = &rest[..end];
    let body = rel.split("<description>").nth(1)?.split("</description>").next()?.trim();
    Some((version, body.to_string()))
}

fn debug_info() -> String {
    format!(
        "termvisor {}\nOS: {} {}\nGTK: {}.{}.{}\nlibadwaita: {}.{}.{}\nInstall: {}\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        gtk::major_version(),
        gtk::minor_version(),
        gtk::micro_version(),
        adw::major_version(),
        adw::minor_version(),
        adw::micro_version(),
        crate::selfupdate::Install::detect().label(),
    )
}

pub fn open(parent: &impl IsA<gtk::Widget>) {
    let d = adw::AboutDialog::builder()
        .application_name("termvisor")
        .application_icon(crate::APP_ID)
        .version(env!("CARGO_PKG_VERSION"))
        .developer_name("Yunus Kurt")
        .developers(["Yunus Kurt"])
        .website("https://yunuskurt.com/termvisor/")
        .issue_url("https://github.com/ykurt52/termvisor/issues")
        .license_type(gtk::License::Gpl30)
        .copyright("© 2026 Yunus Kurt")
        .comments("Serial, SSH, telnet, FTP, Android and USB terminals in one window.")
        .debug_info(debug_info())
        .debug_info_filename("termvisor-debug.txt")
        .build();
    d.add_link("Source Code", "https://github.com/ykurt52/termvisor");
    d.add_link("Releases", "https://github.com/ykurt52/termvisor/releases");
    if let Some((version, notes)) = release_notes(METAINFO) {
        d.set_release_notes_version(&version);
        d.set_release_notes(&notes);
    }
    d.present(Some(parent));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_of_newest_release() {
        let (v, notes) = release_notes(METAINFO).expect("release notes");
        assert!(v.split('.').count() == 3);
        assert!(notes.starts_with("<p>"));
        let xml = r#"<releases><release version="2.0.0" date="x"><description><p>New.</p></description></release><release version="1.0.0"></release></releases>"#;
        assert_eq!(release_notes(xml), Some(("2.0.0".into(), "<p>New.</p>".into())));
    }
}
