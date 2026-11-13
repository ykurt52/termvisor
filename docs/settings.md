# Preferences, shortcuts and the command palette

## Preferences

Menu → Preferences… or Ctrl+, . Changes apply and save at once.

| Page | Contents |
|---|---|
| Appearance | Theme, follow system style, light/dark themes, theme editor, terminal font (fixed-width families) and size, scrollback, renderer (restart), session themes |
| Panes | Defaults for new panes (timestamps, highlight, log to file, log folder), restore tabs, panel on right, F1–F12 command buttons, update check, adb program |
| Keyboard | Shortcut of every window action |
| Files | Opens settings.json, keybindings.json, the themes folder and the configuration folder |

Everything is also plain text in `~/.config/termvisor/`.

## Update check

On every start the main window asks GitHub for the latest release
(`api.github.com/repos/ykurt52/termvisor/releases/latest`, one HTTPS
request). When it is newer than the running version, a toast says
"termvisor X.Y.Z is available"; Download opens the release page. Closing
the toast hides it until the next start. Network errors only go to the
log.

Turn it off with Preferences → Panes → Check for Updates
(`update_check` in `settings.json`, on by default).

## Shortcuts

Preferences → Keyboard: "Set…", then press the keys. Escape cancels,
Backspace removes the shortcut, ↶ restores the default, "Reset All"
restores every one. A function key, or a key with Ctrl, Alt or Super,
is required. A key used by two actions gets a warning.

Only changes are stored, in `~/.config/termvisor/keybindings.json`:

```json
{
  "win.split-right": ["<Ctrl><Alt>Right"],
  "win.copy": []
}
```

An empty list removes the shortcut. Action names are listed in the
command palette and in `src/keybindings.rs`.

| Action | Default |
|---|---|
| New Tab | Ctrl+Shift+T |
| Close Pane | Ctrl+Shift+W |
| Split Right / Down | Ctrl+Shift+E / Ctrl+Shift+O |
| Copy / Paste | Ctrl+Shift+C / Ctrl+Shift+V |
| Zoom In / Out / Reset | Ctrl++ / Ctrl+- / Ctrl+0 |
| Quick Connect | Ctrl+Shift+L |
| Find Session | Ctrl+Shift+K |
| Command Palette | Ctrl+Shift+P |
| Plot Values | Ctrl+Shift+G |
| Toggle Sidebar / Bottom Panel | Ctrl+Shift+B / Ctrl+Shift+J |
| Multi-Exec | Ctrl+Shift+M |
| Preferences | Ctrl+, |
| Release All Connections | Ctrl+Shift+R |

## Command palette

Ctrl+Shift+P lists every window action that can run now, with its
shortcut, and "Theme: …" entries to switch themes. Type to filter
(fuzzy), Up/Down to move, Enter to run.
