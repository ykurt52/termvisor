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

Right-click a button: Send, Edit…, Duplicate, Move Left/Right, Delete.
Drag a button onto another to move it there.

## Pages

`⋮` menu: New Page…, Rename Page…, Delete Page, Import…, Export….
Import adds the pages of a file after the current ones; Export writes all
pages. Deleting the last page leaves an empty "Commands" page.

## File format

`~/.config/termvisor/commands.toml`, shared by all windows. Export files
use the same format. A file that cannot be read is renamed to
`commands.toml.bad` and an empty page is used.

```toml
version = 1

[[page]]
name = "U-Boot"

[[page.button]]
label = "bootargs"
text = """
setenv bootargs console=ttyS0,115200
saveenv"""
eol = "cr"          # cr, lf, crlf, none
delay_ms = 200
color = "yellow"    # blue, green, yellow, red, magenta, cyan

[[page.button]]
label = "MAV heartbeat"
text = "FE 09 00 01 01 00 00 00 00 00 02 03 51 04 03"
hex = true
```
