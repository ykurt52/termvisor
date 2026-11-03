# Windows port

VTE runs only on Unix, so termvisor gets its own terminal view
(`src/vt.rs`) that works wherever GTK runs. `alacritty_terminal` keeps
the screen state (escape sequences, scrollback, selection); the view draws
it with Pango and turns keys and mouse into input.

## Phases

| Phase | Work | State |
|---|---|---|
| 1 | Terminal view, compared with VTE side by side | done |
| 2 | Replace `vte::Terminal` everywhere with the view; VTE dropped | done |
| 3 | Platform layer: ConPTY, Windows Credential Manager, Unix-only parts behind `cfg(unix)` | done, run on Windows |
| 4 | Windows build in CI, zip with the GTK runtime | done, CI builds the zip |
| 5 | Installer with upgrade and downgrade guard | written, waiting for CI |

## Windows differences

| Area | Linux | Windows |
|---|---|---|
| Programs in a pane | own pty (`pty.rs`) | pseudo console (`conpty.rs`), Windows 10 1809 or newer |
| Local shell | `$SHELL` | `pwsh.exe` if installed, else `powershell.exe` |
| Serial ports | `/dev/tty*`, UUCP locks | `COM<n>` (quick connect `COM3@115200`); Windows opens ports exclusively |
| Saved passwords | Secret Service (libsecret) | Credential Manager, entries `termvisor:<id>` |
| SSH | system `ssh`, shared connections (ControlMaster) | Windows OpenSSH `ssh.exe`, no shared connections |
| Ping | `ping` program | ICMP API (the program's output depends on the system language) |
| Traceroute | `tracepath -n` | `tracert -d` |
| Server URLs | all IPv4 addresses | main address and loopback |
| Child cleanup | `PR_SET_PDEATHSIG` | job object that ends its processes when termvisor exits |
| USB capture | usbmon | not available (page hidden) |
| Release All | other instances and their ports | locks and sockets only |
| mosh | yes | no mosh client on Windows |

Platform calls live in `src/sys.rs` (file modes, PATH lookup, parent pid,
tied children, command line quoting) so the rest of the code stays the
same on both.

## Checking the Windows build on Linux

```bash
rustup target add x86_64-pc-windows-gnu
PKG_CONFIG_ALLOW_CROSS=1 cargo clippy --target x86_64-pc-windows-gnu
```

This type-checks the whole program for Windows without the Windows GTK
libraries (pkg-config answers with the Linux ones; nothing is linked).
Run it after changing platform code.

## The view

| Area | Done |
|---|---|
| Drawing | 16/256/true colors, bold, italic, underline, strike, dim, inverse, hidden, wide and combining characters, box drawing and block characters drawn to fill the cell |
| Cursor | block, beam, underline; hollow when not focused; blinks as the desktop setting says |
| Keys | text through the input method (dead keys, compose), arrows, Home/End, PgUp/PgDn, F1-F12, Ctrl and Alt combinations, application cursor mode; Ctrl+Shift is left to the application |
| Mouse | select (double click: word, triple: line, Alt: block), primary selection, middle click paste, wheel scrolls the scrollback (arrow keys in full-screen programs) |
| Mouse reports | programs that ask for the mouse (vim, htop, mc) get clicks, drags, motion and the wheel, SGR or classic form; Shift+mouse still selects |
| Scrollbar | implements `gtk::Scrollable`, so a `ScrolledWindow` shows the scrollback like with VTE |
| Other | Shift+PgUp/PgDn scroll, Shift+Insert paste, bracketed paste, focus reports, title, OSC 52 copy, color and size queries |

Left out because termvisor does not use them with VTE either: search,
hyperlinks, bell.

Method and signal names follow VTE's (`feed`, `commit`, `child-exited`,
`window-title-changed`, `current-directory-uri-changed`, `set_font`,
`set_colors`, ...), so the rest of the code kept its shape. OSC 7 (working
directory) is picked out of the output by the view itself, as the
emulator ignores it; the pty still sets `VTE_VERSION` so shell scripts
like `vte.sh` send it.
