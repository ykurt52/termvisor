# termvisor

Multi-protocol terminal and remote access tool for Linux.
Specification: [docs/SPEC.md](docs/SPEC.md).

## Status

Milestone 1 plus the UI shell:

- Activity bar, side panel with session tree (local shell + hosts from `~/.ssh/config`)
- Quick connect bar (`user@host:port`, `ssh <args>`, `mosh host`, `/dev/ttyUSB0@115200`, `telnet://host`, `tcp://host:port`, `ftp://host`, `adb://serial`)
- Serial, raw TCP and telnet panes: Text / Hex / MAVLink views, ASCII and hex send bar
- Serial locking, mirror and auto-reconnect
- XMODEM, YMODEM and ZMODEM send/receive on serial, TCP and telnet; ZMODEM auto-start and resume
- SFTP and FTP/FTPS file panel, transfer queue with resume, edit remote files locally
- Tabs with nested split panes, start page when no tab is open
- Command panel: pages of up to 50 buttons that send text or hex to the focused pane
- Timestamps, regex highlight rules and logging to file in every pane
- Android: live device list, adb shell, logcat view with filters, device files
- Multi-exec, macros with expect, SSH tunnels (-L/-R/-D) with reconnect
- USB capture (usbmon): decoded transfers, serial view for USB serial adapters, pcap
- Tools: TFTP and HTTP servers, port scanner, ping with a graph, traceroute, Wake-on-LAN
- Preferences window, theme editor with live preview, 9 bundled themes, per-session themes, remappable shortcuts, command palette
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

## Performance

`cargo run` builds a debug binary; use `cargo run --release` (or the
packaged build) for daily use.

Inside a VM without 3D acceleration OpenGL is software (llvmpipe) and GTK's
default renderer is slow. With `"renderer": "auto"` (default in
`settings.json`) termvisor detects this and uses the cairo renderer instead.
Measured on a VMware VM while the terminal scrolls:

| Setup | fps |
|---|---|
| GTK default renderer (ngl), debug build | 13 |
| `auto` → cairo, debug build | 50 |
| `auto` → cairo, release build | 58 |

Force a renderer with `"renderer": "cairo" | "gl" | "ngl" | "vulkan"`, or
the `GSK_RENDERER` environment variable (always wins). Enabling 3D
acceleration in the VM settings gives hardware OpenGL; `auto` then keeps
GTK's GPU renderer.

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
| Multi-exec | Ctrl+Shift+M |
| Command palette | Ctrl+Shift+P |
| Preferences | Ctrl+, |
| Next / previous tab | Ctrl+PgDn / Ctrl+PgUp |
| Go to tab N | Alt+1..9 |

Drag a tab out of the tab bar to open it in a new window.

An SSH pane stays open when the session ends; press Enter to reconnect.
SSH prompts, connection sharing and the keyring: [docs/ssh.md](docs/ssh.md).
Serial ports, locking, mirror and reconnect: [docs/serial.md](docs/serial.md).
TCP, telnet, hex and MAVLink views, send bar: [docs/streams.md](docs/streams.md).
File browser and transfers: [docs/files.md](docs/files.md).
Command buttons: [docs/commands.md](docs/commands.md).
Timestamps, highlight, logging: [docs/output.md](docs/output.md).
Android devices: [docs/adb.md](docs/adb.md).
Multi-exec, macros, tunnels: [docs/multiexec.md](docs/multiexec.md), [docs/macros.md](docs/macros.md), [docs/tunnels.md](docs/tunnels.md).
USB capture: [docs/usb.md](docs/usb.md).
TFTP, HTTP, scanner, ping, Wake-on-LAN: [docs/tools.md](docs/tools.md).
Preferences, shortcuts, command palette: [docs/settings.md](docs/settings.md). Themes and the theme editor: [docs/themes.md](docs/themes.md).
Stuck port or frozen window: [docs/recovery.md](docs/recovery.md), or run `termvisor --release-all`.

## Files

| Path | Content |
|---|---|
| `~/.config/termvisor/settings.json` | Theme, font, scrollback, restore tabs, renderer, pane output defaults |
| `~/.config/termvisor/sessions.toml` | Saved sessions, see [docs/sessions.md](docs/sessions.md) |
| `~/.config/termvisor/commands.toml` | Command panel pages, see [docs/commands.md](docs/commands.md) |
| `~/.config/termvisor/highlight.toml` | Highlight rules, see [docs/output.md](docs/output.md) |
| `~/.config/termvisor/macros.toml`, `tunnels.toml` | Macros and tunnels |
| `~/.config/termvisor/tools.toml` | Tool settings and Wake-on-LAN list |
| `~/.config/termvisor/keybindings.json` | Changed shortcuts, see [docs/settings.md](docs/settings.md) |
| `~/.local/share/termvisor/logs/` | Session logs |
| `~/.local/state/termvisor/recent.json`, `tabs.json` | Recent sessions, saved tabs |
| `~/.config/termvisor/themes/*.json` | User themes, see [docs/themes.md](docs/themes.md) |

## License

Copyright (C) 2026 Yunus Kurt

This program is free software: you can redistribute it and/or modify it
under the terms of the GNU General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option)
any later version. See [LICENSE](LICENSE).
