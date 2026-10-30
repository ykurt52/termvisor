# Themes

A theme is one JSON file. It sets the UI colors and the terminal palette.

## Location

- Builtin: `themes/` in the source tree, compiled into the binary.
- User: `~/.config/termvisor/themes/*.json`, loaded at startup.
  A user theme with the same `name` as a builtin replaces it.

Pick a theme from the main menu → Theme. The choice is saved to
`~/.config/termvisor/settings.json`.

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
