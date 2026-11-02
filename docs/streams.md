# Stream sessions: serial, raw TCP, telnet

Serial ports, raw TCP sockets and telnet share one pane type:

```
[ name  ● connected  23 frames · 0 bad    filter │ View ▾ │ Echo │ CR ▾ │ DTR │ RTS │ Break │ ⟳ │ ⌫ ]
[ terminal                                                                                        ]
[ ASCII ▾ │ send input ...................................................... │ + Enter │ Send ]
```

Serial specifics (locking, mirror, permissions): [serial.md](serial.md).

## Opening

| Session | Quick connect | Start page |
|---|---|---|
| Serial | `/dev/ttyUSB0`, `/dev/ttyS4@921600` | Serial |
| Raw TCP | `tcp://10.0.0.2:5760`, `tcp://[::1]:14550` | Raw TCP |
| Telnet | `telnet://router`, `telnet://10.0.0.1:2323` | Telnet |

Saved sessions: types `serial`, `tcp`, `telnet` in `sessions.toml`.

## Views

| View | Shows |
|---|---|
| Text | Bytes as they come (terminal escapes work) |
| Hex | `00000010  FE 09 00 01 01 00 00 00  00 05 ...  |..........|` 16 bytes per line, offset and ASCII column |
| MAVLink | One line per decoded MAVLink frame, see below |

Switching the view starts a fresh line; nothing already shown is changed.

## Sending

- **Typing** in the terminal sends at once; Enter sends the chosen EOL
  (CR, LF or CRLF).
- **Send bar**, ASCII mode: text with escapes `\r \n \t \0 \\ \xHH`;
  "+ Enter" appends the EOL bytes.
- **Send bar**, Hex mode: `55 AA 01`, `55AA01`, `0x55,0xAA` or `fe:09`.
- Up/Down in the send bar walk the last 50 inputs.
- With Echo on, sent text is shown locally; hex sends show `» 55 AA 01`.

## MAVLink

Frames are found in the stream (v1 `0xFE`, v2 `0xFD`, signed v2 too),
checked with CRC-16/MCRF4XX plus the message CRC_EXTRA, and decoded with
the MAVLink **common** dialect:

```
#17  v1 1:1 HEARTBEAT { custom_mode: 5, mavtype: MAV_TYPE_QUADROTOR, autopilot: MAV_AUTOPILOT_PX4, ... }
```

`#seq  version  system:component  NAME  fields`

- Bytes that are not part of a frame (boot log, NSH text) are counted:
  `[19 non-MAVLink byte(s)]`.
- A frame with a bad CRC is dropped and the decoder resyncs; the toolbar
  counter shows frames and bad CRCs.
- Message ids outside the common dialect are shown with their raw payload
  (their CRC cannot be checked).
- **Filter**: comma separated parts of message names, case-insensitive,
  e.g. `heartbeat, attitude, statustext`. Empty shows all.

Typical PX4 / ArduPilot links:

| Link | Session |
|---|---|
| Telemetry radio / USB | Serial, 57600 or 115200, view MAVLink |
| SITL | Raw TCP `127.0.0.1:5760`, view MAVLink |

Sending MAVLink messages is not built in; raw frames can be sent from the
send bar in Hex mode.

## Telnet

Built-in client, no `telnet` package needed. It accepts the server's ECHO
and SGA options, refuses the rest, and answers each request once. Enter
defaults to CRLF. Break sends telnet BREAK.

## Reconnect

With Auto Reconnect on: serial polls every 500 ms for the device to come
back; TCP and telnet retry every 3 s. Repeated identical errors are shown
once.

## Session format

```toml
[[node]]
type = "tcp"            # or "telnet"
name = "SITL"
host = "127.0.0.1"
port = 5760
eol = "cr"              # cr, lf, crlf
echo = false
hex = false             # start in hex view
mavlink = true          # start in MAVLink view (wins over hex)
reconnect = true
```

Serial sessions take the same `eol`, `echo`, `hex`, `mavlink`, `reconnect`
keys next to their port settings.
