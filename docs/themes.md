# Themes

A theme is one JSON file. It sets the UI colors and the terminal palette.

## Bundled

Dark, Light, High Contrast, Dracula, Solarized Dark, Solarized Light,
Gruvbox, Nord, One Dark.

## Choosing

- Menu → Theme, the command palette ("Theme: …"), or Preferences →
  Appearance.
- **Follow System Style** (Preferences): one theme for a light system,
  another for a dark one; switches when the system does.
- **Session Themes** (Preferences): panes whose title or target matches
  get their own theme, e.g. `root@prod*` → High Contrast, so a production
  shell never looks like a test one. Patterns are comma separated, `*`
  and `?` match.

## Editor

Menu → Theme → Edit Theme… (or Preferences → Edit Theme…). It starts
from the current theme: a bundled one is copied ("… Copy"), a user theme
is edited in place. Every color change shows at once on the whole
window; Cancel or closing the editor puts the old theme back. Save writes
the file and selects it; the trash button deletes a user theme.

## Location

- Bundled: `themes/` in the source tree, compiled into the binary.
- User: `~/.config/termvisor/themes/*.json`. Changes to this folder apply
  at once (no restart). A user theme with the name of a bundled one
  replaces it.

The choice is saved to `~/.config/termvisor/settings.json` (`theme`,
`follow_system`, `theme_light`, `theme_dark`, `theme_rules`).

## Format

```json
{
    "name": "My Theme",
    "kind": "dark",
    "ui": {
        "bg": "#1e1e1e",
        "fg": "#d4d4d4",
        "panel": "#252526",
        "header": "#2d2d2d",
        "accent": "#3794ff",
        "border": "#3c3c3c"
    },
    "term": {
        "fg": "#d4d4d4",
        "bg": "#1e1e1e",
        "cursor": "#aeafad",
        "selection": "#264f78",
        "palette": ["16 colors: black, red, green, yellow, blue, magenta, cyan, white, then bright variants"]
    }
}
```

| Field | Meaning |
|---|---|
| `name` | Name shown in the menu |
| `kind` | `dark` or `light`; sets the base widget style |
| `ui.bg` / `ui.fg` | Window and view background / text |
| `ui.panel` | Popovers, dialogs, side panels |
| `ui.header` | Header bar and tab bar |
| `ui.accent` | Selection and highlighted buttons |
| `ui.border` | Borders and split handles |
| `term.*` | Terminal foreground, background, cursor, selection |
| `term.palette` | Exactly 16 ANSI colors |

Colors accept any CSS color form (`#rrggbb`, `rgb(...)`, names).
An invalid file is skipped and a warning is printed on stderr.
