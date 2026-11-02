# Serial

## Opening a port

- **Quick connect:** `/dev/ttyUSB0` (115200 8N1) or `/dev/ttyS4@921600`.
- **Start page → Serial:** opens the session editor with the first detected
  port filled in; Save adds it to the tree.
- **Session tree:** saved `serial` sessions, see the format below.

The editor's **Device** field has a drop-down of detected ports with their
USB name and VID:PID (e.g. `ttyUSB0 · FT232R USB UART (0403:6001)`).
**Baud Rate** accepts any value; the drop-down lists common ones.

## Pane, views and sending

The serial pane is a stream pane: toolbar, Text/Hex/MAVLink views and the
send bar are described in [streams.md](streams.md). Serial adds DTR, RTS
and a 250 ms Break to the toolbar.

## Locking

A port is opened three ways exclusive:

1. `TIOCEXCL` on the device (other opens fail, root excepted).
2. UUCP lock file `/run/lock/LCK..<name>` with our PID, compatible with
   minicom and picocom. Stale locks (dead PID) are replaced.
3. Inside termvisor: opening a port that another pane owns gives a
   **read-only mirror** of its incoming data. Typing goes to the owner pane.

When a port is busy the pane names the holder, e.g.
`/dev/ttyUSB0 is in use by minicom (PID 4242)`.

## Reconnect

With **Auto Reconnect** on (default), unplugging a USB adapter shows
`[disconnected: …]`, the pane polls every 500 ms and reopens the port with
the same settings when it is back. Permission errors are not retried.

## Permissions

If the port cannot be opened for permission reasons, the pane shows the
command to fix it, e.g.:

```sh
sudo usermod -aG dialout $USER   # Debian/Ubuntu; Arch uses uucp
```

Log out and in afterwards. termvisor never runs it for you.

## Session format

```toml
[[node]]
type = "serial"
name = "UART4"
device = "/dev/ttyS4"
baud = 921600
data_bits = 8          # 5-8
parity = "none"        # none, odd, even
stop_bits = 1          # 1 or 2
flow = "none"          # none, software, hardware
eol = "cr"             # cr, lf, crlf
echo = false
hex = false            # start in hex view
mavlink = false        # start in MAVLink view
reconnect = true
```

Missing fields take the defaults shown.

## Not yet

Per-line timestamps and logging (milestone 9). File transfer with
XMODEM/YMODEM/ZMODEM: see [streams.md](streams.md).
