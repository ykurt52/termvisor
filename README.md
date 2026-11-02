# termvisor

Multi-protocol terminal and remote access tool for Linux.
Specification: [docs/SPEC.md](docs/SPEC.md).

## Status

Milestone 1 plus the UI shell:

- Activity bar, side panel with session tree (local shell + hosts from `~/.ssh/config`)
- Quick connect bar (`user@host:port`, `ssh <args>`, `mosh host`, `/dev/ttyUSB0@115200`, `telnet://host`, `tcp://host:port`)
- Serial, raw TCP and telnet panes: Text / Hex / MAVLink views, ASCII and hex send bar
- Serial locking, mirror and auto-reconnect
- Tabs with nested split panes, start page when no tab is open
- Bottom dock and status bar
- Live theme switching

Panels marked with a milestone number are placeholders.

## Build

Dependencies (Debian/Ubuntu):

```sh
sudo apt install libgtk-4-dev libadwaita-1-dev libvte-2.91-gtk4-dev libsecret-1-dev libudev-dev build-essential pkg-config
```

Dependencies (Arch):

```sh
sudo pacman -S gtk4 libadwaita vte4 libsecret base-devel
```

Optional on both: `mosh` for mosh sessions.

Rust 1.92 or newer (via rustup), then:

```sh
cargo build --release
./target/release/termvisor
```

## Shortcuts

| Action | Keys |
|---|---|
| New tab | Ctrl+Shift+T |
| Close pane | Ctrl+Shift+W |
| Split right | Ctrl+Shift+E |
| Split down | Ctrl+Shift+O |
| Copy / Paste | Ctrl+Shift+C / Ctrl+Shift+V |
| Zoom in / out / reset | Ctrl++ / Ctrl+- / Ctrl+0 |
| Quick connect | Ctrl+Shift+L |
| Find session | Ctrl+Shift+K |
| Release all connections | Ctrl+Shift+R |
| Toggle sidebar | Ctrl+Shift+B |
| Toggle bottom panel | Ctrl+Shift+J |
| Next / previous tab | Ctrl+PgDn / Ctrl+PgUp |
| Go to tab N | Alt+1..9 |

Drag a tab out of the tab bar to open it in a new window.

An SSH pane stays open when the session ends; press Enter to reconnect.
SSH prompts, connection sharing and the keyring: [docs/ssh.md](docs/ssh.md).
Serial ports, locking, mirror and reconnect: [docs/serial.md](docs/serial.md).
TCP, telnet, hex and MAVLink views, send bar: [docs/streams.md](docs/streams.md).
Stuck port or frozen window: [docs/recovery.md](docs/recovery.md), or run `termvisor --release-all`.

## Files

| Path | Content |
|---|---|
| `~/.config/termvisor/settings.json` | Theme, font, scrollback, restore tabs |
| `~/.config/termvisor/sessions.toml` | Saved sessions, see [docs/sessions.md](docs/sessions.md) |
| `~/.local/state/termvisor/recent.json`, `tabs.json` | Recent sessions, saved tabs |
| `~/.config/termvisor/themes/*.json` | User themes, see [docs/themes.md](docs/themes.md) |

## License

Copyright (C) 2026 Yunus Kurt

This program is free software: you can redistribute it and/or modify it
under the terms of the GNU General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option)
any later version. See [LICENSE](LICENSE).
