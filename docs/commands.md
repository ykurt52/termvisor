# Command panel

Bottom dock → **Commands**. Pages of up to 50 buttons; a click sends the
button's payload to the focused pane.

```
[ U-Boot ▾ ][ ⋮ ][ + ]  5/50        click sends · right-click edits · drag reorders
[ printenv ][ bootargs ][ reset ][ MAV heartbeat ][ echo hi ][ + ]
```

## Where a click goes

The pane that has the keyboard focus, else the first pane of the selected
tab. Buttons do not take the focus, so you can type and click in turn.

| Pane | What happens |
|---|---|
| Serial, raw TCP, telnet | Bytes go to the link as they are (binary safe). With Echo on, text is shown, hex shows `» 55 AA 01`. Paused while a file transfer runs. |
| Shell, SSH, mosh | Bytes go to the program as typed input. |

## Button

| Field | Meaning |
|---|---|
| Label | Text on the button; empty uses the first payload line |
| Payload Type | **Text**: one command per line, escapes `\r \n \t \0 \\ \xHH`. **Hex bytes**: `55 AA 01`, `55AA01`, `0x55,0xAA`, sent as one block |
| Line Ending | Added after each text line: CR, LF, CRLF or None |
| Delay Between Lines | Pause between text lines, in ms (e.g. U-Boot `setenv` then `saveenv`) |
| Color | Button tint |
| Shortcut | Set… then press the keys; Escape cancels. A function key, or any key with Ctrl, Alt or Super. Shown on the button |

Right-click a button: Send, Edit…, Duplicate, Move Left/Right, Delete.
Drag a button onto another to move it there.

## Keyboard

Shortcuts work while a terminal has the focus; dialogs and text fields
keep their keys. They apply to the page that is shown.

- **Own shortcut** per button (editor → Shortcut).
- **F1–F12** (`⋮` → "F1–F12 Send Buttons 1–12", off by default): F1 sends
  button 1, and so on. Buttons with their own shortcut keep it. Off by
  default because programs like mc and htop use the function keys.
  Stored as `"command_keys"` in `settings.json`.

## Pages

`⋮` menu: New Page…, Page Settings…, Delete Page, Import…, Export….
Import adds the pages of a file after the current ones; Export writes all
pages. Deleting the last page leaves an empty "Commands" page.

**Page for a session.** Page Settings → Sessions takes comma separated
patterns (`*` any text, `?` one character, case ignored). When a pane
whose title or target matches gets the focus, its page is shown:

| Pattern | Matches |
|---|---|
| `U-Boot*` | Sessions named "U-Boot …" |
| `/dev/ttyUSB*` | Any USB serial port |
| `root@10.0.0.*` | SSH to that subnet as root |
| `*:5760` | TCP/telnet to port 5760 |

The first matching page wins; if the shown page matches, it stays.

## Placement

Menu → "Panel on Right" moves the dock (commands, transfers, log) to
the right of the tabs (`"dock_right"` in `settings.json`). Ctrl+Shift+J
hides or shows it.

## File format

`~/.config/termvisor/commands.toml`, shared by all windows. Export files
use the same format. A file that cannot be read is renamed to
`commands.toml.bad` and an empty page is used.

```toml
version = 1

[[page]]
name = "U-Boot"
sessions = ["U-Boot*", "/dev/ttyUSB*"]

[[page.button]]
label = "bootargs"
text = """
setenv bootargs console=ttyS0,115200
saveenv"""
eol = "cr"          # cr, lf, crlf, none
delay_ms = 200
color = "yellow"    # blue, green, yellow, red, magenta, cyan
key = "<Control>1"  # GTK accelerator; "F5", "<Alt>r", ...

[[page.button]]
label = "MAV heartbeat"
text = "FE 09 00 01 01 00 00 00 00 00 02 03 51 04 03"
hex = true
```
