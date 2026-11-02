# Macros

Steps that type text, wait, or wait for output, run on a pane.
Side panel → Macros (record icon).

| Step | Does |
|---|---|
| Send | Types the text. Escapes: `\r` Enter, `\n`, `\t`, `\\`, `\xHH` (e.g. `\x03` Ctrl+C) |
| Wait | Pauses, in milliseconds |
| Expect | Waits until the output since the last Send matches a regex; fails after the timeout (seconds) |

Expect uses Rust regex syntax on the output without colors; `^` and `$`
match at line starts and ends. Example for U-Boot over serial:

| # | Step | Value |
|---|---|---|
| 1 | Send | `\r` |
| 2 | Expect | `=> $` (10 s) |
| 3 | Send | `setenv bootargs console=ttyS0,115200\r` |
| 4 | Expect | `=> $` |
| 5 | Send | `saveenv\r` |

## Run

- ▶ on a row: the focused pane, or every selected pane in multi-exec.
- Shortcut (editor → Shortcut → Set…): works while a terminal has the
  focus.
- Sessions (editor): comma separated patterns on the pane title or target
  (`/dev/ttyUSB*`, `root@10.0.0.*`); the macro refuses other panes. Empty:
  every pane.
- A toast tells when it is done or why it stopped. ■ Stop ends running
  macros.

## Record

● Record, type in the focused pane, ■ Stop recording. Each line becomes a
Send step; a pause of a second or more becomes a Wait. The editor opens
to name it, add Expect steps and save.

## File

`~/.config/termvisor/macros.toml`:

```toml
version = 1

[[macro]]
name = "save bootargs"
key = "<Alt>b"
sessions = ["/dev/ttyUSB*"]

[[macro.step]]
do = "send"
text = "\\r"

[[macro.step]]
do = "expect"
pattern = "=> $"
timeout_ms = 10000

[[macro.step]]
do = "wait"
ms = 500
```
