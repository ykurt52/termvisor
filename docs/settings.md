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
"termvisor X.Y.Z is available". Closing the toast hides it until the next
start. Network errors only go to the log.

| Install | Button | What it does |
|---|---|---|
| Windows installer | Update | downloads `_setup.exe`, checks its SHA-256 against the release, runs it silently (`/SILENT /relaunch=1`) and closes; the installer starts termvisor again |
| Windows portable exe | Update | downloads `_portable.exe` next to the old one, checks its SHA-256, deletes the old exe (a renamed one is replaced in place) and starts the new one |
| deb, AUR, own build | Download | opens the release page; the package manager updates packages |

With open sessions it asks first, since termvisor restarts. A failed
download or check ends in a toast with the release page; nothing is
changed then. The kind of install is detected from the exe: the
uninstaller next to it means installer, `TERMVISOR_PORTABLE` (set by the
portable launcher) means portable.

To try the update path against the current release, start termvisor with
`TERMVISOR_PRETEND_VERSION=1.0.0`; it then offers the latest release.

Turn it off with Preferences → Panes → Check for Updates
(`update_check` in `settings.json`, on by default).

## About

Menu → About termvisor: version, publisher, website, license, the notes
of the newest release and, under Troubleshooting, the OS, GTK version and
install kind to paste into a bug report.

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
