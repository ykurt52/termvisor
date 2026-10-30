// Theme files: parsing, validation and CSS generation.

use std::path::Path;

use gtk::gdk;
use serde::Deserialize;

const BUILTIN: [&str; 5] = [
    include_str!("../themes/dark.json"),
    include_str!("../themes/light.json"),
    include_str!("../themes/dracula.json"),
    include_str!("../themes/gruvbox.json"),
    include_str!("../themes/nord.json"),
];

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Dark,
    Light,
}

#[derive(Deserialize)]
struct UiFile {
    bg: String,
    fg: String,
    panel: String,
    header: String,
    accent: String,
    border: String,
}

#[derive(Deserialize)]
struct TermFile {
    fg: String,
    bg: String,
    cursor: String,
    selection: String,
    palette: Vec<String>,
}

#[derive(Deserialize)]
struct ThemeFile {
    name: String,
    kind: Kind,
    ui: UiFile,
    term: TermFile,
}

/// Terminal colors, ready for VTE.
#[derive(Clone)]
pub struct Colors {
    pub fg: gdk::RGBA,
    pub bg: gdk::RGBA,
    pub cursor: gdk::RGBA,
    pub selection: gdk::RGBA,
    pub palette: [gdk::RGBA; 16],
}

/// Validated theme.
#[derive(Clone)]
pub struct Theme {
    pub name: String,
    pub dark: bool,
    pub css: String,
    pub colors: Colors,
}

fn rgba(s: &str) -> Result<gdk::RGBA, String> {
    gdk::RGBA::parse(s).map_err(|_| format!("invalid color '{s}'"))
}

fn build(file: ThemeFile) -> Result<Theme, String> {
    if file.term.palette.len() != 16 {
        return Err(format!("palette needs 16 colors, got {}", file.term.palette.len()));
    }
    let mut palette = [gdk::RGBA::BLACK; 16];
    for (slot, s) in palette.iter_mut().zip(&file.term.palette) {
        *slot = rgba(s)?;
    }
    let colors = Colors {
        fg: rgba(&file.term.fg)?,
        bg: rgba(&file.term.bg)?,
        cursor: rgba(&file.term.cursor)?,
        selection: rgba(&file.term.selection)?,
        palette,
    };

    // Colors are re-serialized from parsed values so file content never reaches CSS raw.
    let ui = &file.ui;
    let bg = rgba(&ui.bg)?.to_string();
    let fg = rgba(&ui.fg)?.to_string();
    let panel = rgba(&ui.panel)?.to_string();
    let header = rgba(&ui.header)?.to_string();
    let accent = rgba(&ui.accent)?.to_string();
    let border = rgba(&ui.border)?.to_string();
    let pal: Vec<String> = colors.palette.iter().map(|c| c.to_string()).collect();
    let (red, green, yellow, blue, magenta, cyan) =
        (&pal[9], &pal[10], &pal[11], &pal[12], &pal[13], &pal[14]);
    let css = format!(
        "@define-color c_red {red};
@define-color c_green {green};
@define-color c_yellow {yellow};
@define-color c_blue {blue};
@define-color c_magenta {magenta};
@define-color c_cyan {cyan};
@define-color window_bg_color {bg};
@define-color window_fg_color {fg};
@define-color view_bg_color {bg};
@define-color view_fg_color {fg};
@define-color headerbar_bg_color {header};
@define-color headerbar_fg_color {fg};
@define-color headerbar_backdrop_color {header};
@define-color sidebar_bg_color {panel};
@define-color sidebar_fg_color {fg};
@define-color card_bg_color {panel};
@define-color card_fg_color {fg};
@define-color popover_bg_color {panel};
@define-color popover_fg_color {fg};
@define-color dialog_bg_color {panel};
@define-color dialog_fg_color {fg};
@define-color accent_bg_color {accent};
@define-color accent_color {accent};
@define-color borders {border};
tabbar .box {{ background-color: {header}; }}
paned > separator {{ background-color: {border}; }}
"
    );

    Ok(Theme {
        name: file.name,
        dark: matches!(file.kind, Kind::Dark),
        css,
        colors,
    })
}

fn parse(text: &str, src: &str) -> Option<Theme> {
    let res = serde_json::from_str::<ThemeFile>(text)
        .map_err(|e| e.to_string())
        .and_then(build);
    match res {
        Ok(t) => Some(t),
        Err(e) => {
            crate::warn(&format!("theme {src}: {e}"));
            None
        }
    }
}

fn add(list: &mut Vec<Theme>, t: Theme) {
    match list.iter_mut().find(|x| x.name == t.name) {
        Some(slot) => *slot = t,
        None => list.push(t),
    }
}

/// Builtin themes, then user themes from `dir` (same name overrides).
pub fn load(dir: &Path) -> Vec<Theme> {
    let mut list = Vec::new();
    for (i, text) in BUILTIN.iter().enumerate() {
        if let Some(t) = parse(text, &format!("builtin #{i}")) {
            add(&mut list, t);
        }
    }

    let Ok(entries) = std::fs::read_dir(dir) else {
        return list;
    };
    let mut paths: Vec<_> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    for path in paths {
        let src = path.display().to_string();
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                if let Some(t) = parse(&text, &src) {
                    add(&mut list, t);
                }
            }
            Err(e) => crate::warn(&format!("theme {src}: {e}")),
        }
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_parse() {
        for text in BUILTIN {
            let file: ThemeFile = serde_json::from_str(text).expect("json");
            build(file).expect("theme");
        }
    }

    #[test]
    fn short_palette_fails() {
        let text = BUILTIN[0].replacen(", \"#ffffff\"", "", 1);
        let file: ThemeFile = serde_json::from_str(&text).expect("json");
        assert!(build(file).is_err());
    }
}
