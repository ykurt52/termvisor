# Timestamps, highlight and logging

Every pane (shell, SSH, mosh, serial, TCP, telnet) passes its output
through the same pipeline before it reaches the screen:

```
child / link  →  log file  →  timestamps + highlight  →  screen
```

Controls: bottom dock → **Log**. They act on the focused pane (or the
last focused one when the focus is on the dock).

```
[ root@board                 ]  │ Highlight Rules                 ↶  +
[ Timestamps ][ Highlight ]     │ ☑ \b(error|fail|...)\b      Aa  ↑ ✎ 🗑
  Log to file [ Plain text ▾ ] 📂│ ☑ \b(warn|warning)\b       Aa  ↑ ✎ 🗑
  ~/.local/share/termvisor/logs/root_board-20261102-141530.log
```

## Timestamps

`[14:15:30.117]` before each line, dimmed. The time is when the bytes
arrived; lines that arrive in one read share a time.

## Highlight

Regex rules (Rust `regex` syntax) color their matches. Rules are shared
by all panes and stored in `~/.config/termvisor/highlight.toml`.

| Default rule | Color |
|---|---|
| `\b(error\|errors\|fail\|failed\|failure\|fatal\|panic\|exception\|denied\|refused)\b` | red, bold |
| `\b(warn\|warning\|warnings\|deprecated)\b` | yellow |
| `\b(ok\|done\|success\|successful\|pass\|passed)\b` | green |
| IPv4 address | cyan |

- Earlier rules win where matches overlap (↑ moves a rule up).
- Colors the program already set stay; after a match they are restored.
- Text inside escape sequences (titles, OSC) is never changed.
- ↶ restores the default rules.
- A word split across two reads (common on slow serial lines) may stay
  uncolored; the rest of the line is still checked.

```toml
version = 1

[[rule]]
pattern = '\bkernel panic\b'
color = "red"           # red, green, yellow, blue, magenta, cyan
bold = true
ignore_case = true
enabled = true
```

## Full-screen programs

While a program uses the alternate screen (vim, htop, mc, less),
timestamps and highlight are off, so its screen is never changed. The
log still records everything.

## Logging

| Mode | File content |
|---|---|
| Off | No file |
| Raw | Bytes as received, including escape sequences (replay with `cat`) |
| Plain text | Escape sequences and CR removed; timestamps when they are on |

- Folder: `~/.local/share/termvisor/logs/`, or `log_dir` in settings. 📂
  opens it.
- File: `<session>-<YYYYmmdd-HHMMSS>.log`, mode 0600, appended.
- Written after every read, so a crash loses nothing.
- Serial/TCP/telnet in Hex or MAVLink view: the log gets the raw bytes.

## Defaults for new panes

`~/.config/termvisor/settings.json`:

```json
{
  "timestamps": false,
  "highlight": true,
  "log": "off",
  "log_dir": ""
}
```

`log` is `off`, `raw` or `plain`. The Log page changes only the focused
pane.

## How shells and SSH are run

termvisor opens the pty itself (`openpty`), starts the program on it as
a new session, and reads the output in a thread. Typed text, pastes,
command buttons and terminal replies (cursor position, device
attributes) go back to the program. The environment gets
`TERM=xterm-256color`, `COLORTERM=truecolor` and `VTE_VERSION`, so shell
integration (directory reporting for the Files panel) works as before.

Throughput, `seq 1 1000000` in a shell (release build, cairo renderer):

| Setup | Time |
|---|---|
| Before (pty owned by the terminal widget) | 0.80–0.94 s |
| Own pty, highlight on | 0.70–0.73 s |
| Own pty, highlight + timestamps | 0.95 s |
| Own pty, highlight + timestamps + plain log | 1.04 s |
