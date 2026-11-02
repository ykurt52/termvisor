# termvisor — Specification

Working name. Final name pending a trademark check.

## 0. Goals

- Multi-protocol terminal and remote access tool for Linux.
- Runs on Debian/Ubuntu and Arch without extra setup.
- Never takes down other sessions when one connection fails.
- Fully themeable, keyboard driven.
- Open source, GPL-3.0-or-later.

## 1. Stack

| Layer | Choice | Reason |
|---|---|---|
| Language | Rust (stable) | No GC, memory safe, C-level speed |
| GUI | GTK4 + libadwaita | Native on Debian and Arch, CSS theming |
| Terminal | VTE (gtk4) | Mature emulator, truecolor, hyperlinks |
| SSH | System OpenSSH + ControlMaster | Reuses `~/.ssh/config`, agent, ProxyJump, FIDO |
| SFTP | SFTP subsystem over the master socket | No second login |
| Serial | `serialport` crate | Fed into VTE via `feed()` |
| FTP/FTPS | `suppaftp` | TLS support |

Minimum runtime: GTK 4.14, libadwaita 1.5, VTE 0.76 (Ubuntu 24.04 / Mint 22 baseline).
Code must not use APIs newer than these.

## 2. Connection types

| Type | Implementation | Version |
|---|---|---|
| Local shell | VTE + pty | v1 |
| SSH | OpenSSH + ControlMaster | v1 |
| Serial | `serialport` | v1 |
| Telnet | Built-in client | v1 |
| Mosh | System `mosh` (optional) | v1 |
| Raw TCP | Built-in | v1 |
| FTP / FTPS | `suppaftp` | v1 |
| ADB | System `adb` (optional) | v1 |
| RDP | FreeRDP | v2 |
| VNC | gtk-vnc | v2 |

## 3. Tabs, panes, isolation

- Unlimited tabs, mixed types.
- Split panes, horizontal and vertical, nested. Drag panes between tabs.
- Detach a tab into a new window.
- Each connection runs on its own thread. A failing connection only marks its
  own pane as disconnected ("Enter to reconnect").
- Serial port lock: one writer per port. Other panes may open a read-only mirror.
- Saved layouts (workspaces) restore tabs, panes and their sessions.

## 4. Resource release and recovery

Details in docs/recovery.md.

- A crash needs no child reaping: ptys hang up (SIGHUP to shells and ssh)
  and the kernel closes serial fds. `PR_SET_PDEATHSIG` is not used: it
  tracks the forking thread, not the process.
- Startup removes stale UUCP locks (dead PID) and stale ControlMaster
  sockets, and reports it in a toast.
- "Release All" (menu + Ctrl+Shift+R), after confirmation: closes this
  process's serial ports (panes stay), closes shared SSH connections,
  removes stale locks/sockets, optionally stops other instances
  (SIGTERM, then SIGKILL).
- Port picker / open errors name the process holding a busy port.
  Foreign processes are only reported, never killed.
- CLI: `termvisor --release-all` does the same without the GUI.
- Watchdog: if the GUI thread stalls for more than 5 s, a helper process
  offers "Wait" or "Stop and Release".

## 5. Session management

- Tree with nested folders, drag and drop, color and icon tags.
- Storage: `~/.config/termvisor/sessions.toml`. No passwords in this file.
- Folder settings inherit to children (user, jump host, theme, etc.).
- Quick connect bar: `user@host:port`, Enter connects without saving.
- Fuzzy search (Ctrl+Shift+K; plain Ctrl+K stays with the shell).
- Import: hosts from `~/.ssh/config`, third-party INI-based session exports.
- Export: TOML, JSON.
- Recent and favorites sections.
- Optional restore of open tabs on startup.

## 6. Terminal

- Truecolor, font selection, ligatures, configurable or unlimited scrollback.
- Regex search in scrollback.
- Highlight rules: regex to color, per profile.
- Per-line timestamps (toggle).
- Per-session logging to file, raw or ANSI-stripped.
- Copy on select, right-click paste (optional). Confirm multi-line paste.
- Clickable URLs and file paths.

Binary protocols (serial, TCP, telnet):

- Views: Text, Hex (offset + ASCII column), MAVLink (common dialect,
  CRC checked, name filter).
- Send bar: ASCII with C escapes or hex bytes, input history.

Serial extras:

- Hex view.
- Line ending: CR, LF, CRLF.
- Local echo, send break, DTR/RTS toggles.
- XMODEM, YMODEM, ZMODEM (send and receive). Auto-detect incoming `rz`/`sz`.
- Auto-reconnect when a USB serial device is unplugged and replugged.

## 7. File transfer

Browser panel:

- Opens automatically with an SSH session, shares its master socket.
- Optional follow of the terminal working directory (OSC 7 / prompt hook).
- List and tree views; size, date, mode, owner columns; hidden files toggle.
- Rename, delete, mkdir, chmod dialog, symlink follow.
- Path bar and bookmarks.

Transfers:

- Drag and drop upload and download.
- Queue panel: progress, speed, ETA, pause, resume, cancel.
- Resume of interrupted transfers.
- Parallel transfers (default 4).
- Recursive folders.
- Conflict dialog: overwrite, skip, rename, overwrite if newer; apply to all.
- Optional post-transfer `sha256sum` check.
- Edit remote file in a local editor, auto-upload on save.
- Dual pane: local–remote and remote–remote.
- FTP/FTPS uses the same browser and queue; active/passive mode.
- Serial XYZMODEM transfers use the same queue.

## 8. Network tools

Tunnels:

- Local (`-L`), remote (`-R`), dynamic SOCKS (`-D`).
- Standalone tunnels without a terminal.
- List with status, traffic counters, start/stop.
- Auto-reconnect, start on launch.
- Jump host chains.

Multi-exec:

- Broadcast input to selected tabs/panes.
- Targets picked by checkbox; target panes get a red border.
- Single-shot "send to selected" input field.

Macros:

- Record and replay keystrokes.
- Scripted steps with delays and expect conditions (wait for output pattern).
- Trigger by shortcut or toolbar button.
- Global or per session.

Tools menu:

- TCP port scanner.
- Ping / traceroute with a graph.
- Wake-on-LAN.
- TFTP server and HTTP file server, one-click start/stop.

## 9. Command panel

- Pages of up to 50 buttons each; multiple pages.
- Button: label, payload, line ending (CR, LF, CRLF, none), color.
- Multi-line payload with per-line delay (ms).
- Hex payload mode (`0x55 0xAA 0x01`).
- Click sends to the active pane, or to all multi-exec targets when active.
- Shortcuts: F1–F12 for the first 12 buttons, others assignable.
- Global or per-session pages; a session can open a default page.
- Dock bottom or right, hide, reorder by drag.
- Import/export pages as TOML.

## 10. UI and themes

Layout:

- Activity bar: Sessions, Files, Tunnels, Macros, Tools.
- Center: tabs and split panes.
- Bottom dock: command panel, transfer queue, log.
- Status bar: connection type, host/port, baud, RX/TX counters, encoding, latency.
- Command palette (Ctrl+Shift+P).

Themes:

- JSON theme files covering UI colors and the terminal palette
  (16 colors, fg, bg, cursor, selection).
- Bundled: Dark, Light, High Contrast, plus openly licensed palettes
  (Dracula, Solarized, Gruvbox, Nord, One Dark).
- User themes in `~/.config/termvisor/themes/`, hot reload.
- In-app theme editor with live preview.
- Follow system dark/light.
- Per-session theme override.

Settings:

- GUI editor and raw `settings.json`.
- Fully remappable shortcuts in `keybindings.json`.

Localization: English default, i18n ready.

## 11. Security

- Passwords in the system keyring (libsecret). Never in our files.
- Fallback vault when no keyring: Argon2id + XChaCha20-Poly1305, master
  password, auto-lock after idle timeout.
- Per-session "never store, always ask".
- Key manager: generate (ed25519, RSA, ECDSA), list, change passphrase,
  copy public key, install to remote `authorized_keys`.
- ssh-agent view: list, add, remove keys.
- Host keys via `~/.ssh/known_hosts`. Unknown host: fingerprint dialog.
  Changed host key: blocking red warning.
- Passwords masked in logs; pasted secrets not written to scrollback logs.
- Config and log files created with mode `0600`.
- No telemetry. Update check optional, off by default.
- Plaintext protocols (Telnet, FTP) show an unlocked icon on the tab.

## 12. Packaging

| Channel | Tool | Notes |
|---|---|---|
| `.deb` | `cargo-deb` | GitHub Releases, amd64 + arm64 |
| apt repo | PPA or self-hosted | `apt install` / `apt upgrade` |
| Arch | AUR `termvisor`, `termvisor-bin` | `PKGBUILD` in repo |
| Debian official | `debian/` | Long term; prefer crates packaged in Debian |
| AppImage | optional | Other distributions |

Flatpak is not supported: sandbox blocks serial ports, host `ssh` and `~/.ssh`.

Dependencies: `openssh-client` (required); `mosh`, `freerdp`, `gtk-vnc` optional.
Missing optional tools disable their menu entries.

Serial permission: if the user is not in `dialout` (Debian) or `uucp` (Arch),
show a warning with the command to fix it. Never run it automatically.

CI (GitHub Actions): clippy, tests, `cargo-audit`, builds on Ubuntu 24.04 and
Arch containers, release packages on tag.

## 13. Code rules

- English only in code, comments, docs and logs.
- No other product names in code.
- Short, clear names: `pull_term`, not `get_and_fetch_serial_terminal`.
- 4-space indent, no tabs. `rustfmt` and `clippy` clean.
- No `unwrap()`/`expect()` outside tests; errors are propagated and shown.
- Panics in connection threads are caught and reported per pane.

## 14. Milestones (v1)

1. Skeleton: window, tabs, split panes, local shell, theme loader.
2. Session tree and storage, quick connect.
3. SSH with ControlMaster, host key handling, keyring.
4. Serial: port picker, lock, mirror, hex view, line endings, reconnect.
5. Recovery: startup cleanup, Release All, `--release-all`, watchdog.
6. Telnet, Raw TCP, Mosh; binary send bar, hex+ASCII view, MAVLink decoder.
7. File browser and transfer queue: 7a SFTP browser + queue, 7b FTP/FTPS,
   remote edit, follow cwd, resume (dual pane left for later).
8. XMODEM / YMODEM / ZMODEM (ZMODEM via the zmodem2 crate, resume, auto-start).
9. Command panel, highlight rules, timestamps, logging.
10. ADB: device list in the sidebar (`adb devices -l`, live), `adb shell`
    sessions per serial, logcat view with level/tag/PID filters and colors,
    file panel over `adb push`/`pull`, reconnect on replug.
11. Tunnels, multi-exec, macros.
12. Tools: TFTP, HTTP server, scanner, ping, WOL.
13. Theme editor, settings UI, keybindings.
14. Packaging and CI.

## Backlog

- Upload & run (may become a macro template).
- RDP, VNC (v2).
