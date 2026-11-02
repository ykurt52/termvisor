# Multi-exec

Type once, send to several panes: shells, SSH, serial, TCP, telnet, adb.

Start: header grid button, menu → Multi-Exec, or Ctrl+Shift+M. The
focused pane is selected; each pane gets a "Multi-exec" checkbox at its
top right. Selected panes have a red frame.

```
[● Multi-exec  3 panes selected  [ line for the selected panes......] Send  All  None  ✕ ]
```

| How | Goes to |
|---|---|
| Typing in a selected pane | Every selected pane |
| The bar's line + Enter or Send | Every selected pane, with Enter |
| Command panel button | Every selected pane (instead of the focused one) |
| Macro Run | Runs on every selected pane, each with its own Expect |

- Serial, TCP and telnet panes turn Enter into their own line ending
  (CR, LF or CRLF) and echo when Echo is on.
- Replies a terminal sends by itself (cursor position, device
  attributes, focus events, color queries) stay in their pane, so a
  full-screen program in one pane does not type into the others.
- Typing in a pane that is not selected stays in that pane.
- Ctrl+Shift+M again, or ✕, turns it off; the selection is kept for next
  time.
