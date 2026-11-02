// Theme files: parsing, validation and CSS generation.

use std::fs;
use std::path::{Path, PathBuf};

use gtk::gdk;
use serde::{Deserialize, Serialize};

const BUILTIN: [&str; 9] = [
    include_str!("../themes/dark.json"),
    include_str!("../themes/light.json"),
    include_str!("../themes/high-contrast.json"),
    include_str!("../themes/dracula.json"),
    include_str!("../themes/solarized-dark.json"),
    include_str!("../themes/solarized-light.json"),
    include_str!("../themes/gruvbox.json"),
    include_str!("../themes/nord.json"),
    include_str!("../themes/one-dark.json"),
];

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Dark,
    Light,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UiFile {
    pub bg: String,
    pub fg: String,
    pub panel: String,
    pub header: String,
    pub accent: String,
    pub border: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TermFile {
    pub fg: String,
    pub bg: String,
    pub cursor: String,
    pub selection: String,
    pub palette: Vec<String>,
}

/// A theme as stored in JSON.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThemeFile {
    pub name: String,
    pub kind: Kind,
    pub ui: UiFile,
    pub term: TermFile,
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
    /// Source, for the editor.
    pub file: ThemeFile,
    /// File of a user theme; None for the bundled ones.
    pub path: Option<PathBuf>,
}

fn rgba(s: &str) -> Result<gdk::RGBA, String> {
    gdk::RGBA::parse(s).map_err(|_| format!("invalid color '{s}'"))
}

/// Checks the colors and makes the CSS.
pub fn build(file: ThemeFile) -> Result<Theme, String> {
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

    if file.name.trim().is_empty() {
        return Err("a theme needs a name".into());
    }
    Ok(Theme { name: file.name.clone(), dark: matches!(file.kind, Kind::Dark), css, colors, file, path: None })
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
                if let Some(mut t) = parse(&text, &src) {
                    t.path = Some(path.clone());
                    add(&mut list, t);
                }
            }
            Err(e) => crate::warn(&format!("theme {src}: {e}")),
        }
    }
    list
}

/// File name for a user theme called `name`.
fn file_name(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    let s = s.trim_matches('-').to_string();
    format!("{}.json", if s.is_empty() { "theme" } else { &s })
}

/// Writes a user theme; returns its path. `old` is the file it replaces.
pub fn save(dir: &Path, file: &ThemeFile, old: Option<&Path>) -> Result<PathBuf, String> {
    build(file.clone())?;
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let path = dir.join(file_name(&file.name));
    let text = serde_json::to_string_pretty(file).map_err(|e| e.to_string())?;
    fs::write(&path, text + "\n").map_err(|e| e.to_string())?;
    if let Some(o) = old.filter(|o| *o != path) {
        let _ = fs::remove_file(o);
    }
    Ok(path)
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
    fn user_theme_roundtrip() {
        let dir = std::env::temp_dir().join(format!("tv-theme-{}", std::process::id()));
        let mut f: ThemeFile = serde_json::from_str(BUILTIN[0]).expect("json");
        f.name = "My Prod/Red".into();
        f.ui.accent = "#ff0000".into();
        let p = save(&dir, &f, None).expect("save");
        assert_eq!(p.file_name().and_then(|n| n.to_str()), Some("my-prod-red.json"));
        let list = load(&dir);
        let t = list.iter().find(|t| t.name == "My Prod/Red").expect("loaded");
        assert_eq!(t.file, f);
        assert_eq!(t.path.as_deref(), Some(p.as_path()));
        assert!(list.iter().filter(|t| t.path.is_none()).count() == BUILTIN.len());
        // Renaming replaces the old file.
        f.name = "Other".into();
        let q = save(&dir, &f, Some(&p)).expect("save");
        assert!(!p.exists() && q.exists());
        f.term.fg = "nope".into();
        assert!(save(&dir, &f, None).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn short_palette_fails() {
        let text = BUILTIN[0].replacen(", \"#ffffff\"", "", 1);
        let file: ThemeFile = serde_json::from_str(&text).expect("json");
        assert!(build(file).is_err());
    }
}
