# USB capture

Listens to USB traffic between this computer and its devices, like the
paid USB sniffers on Windows. It uses the kernel's `usbmon`, which hands
over a copy of each transfer; the traffic itself is not touched or slowed.

Side panel → USB.

## One-time setup

usbmon is a kernel module and its files are readable only by root,
because they show all USB traffic, keyboards included. "Enable USB
Capture…" asks for the administrator password (pkexec) once and:

| File | Content |
|---|---|
| `/etc/modules-load.d/termvisor-usbmon.conf` | `usbmon`, loaded at every boot |
| `/etc/udev/rules.d/70-termvisor-usbmon.rules` | `SUBSYSTEM=="usbmon", TAG+="uaccess"`: the user logged in at the screen may read `/dev/usbmon*`, nobody else |

It then loads the module and applies the rule; no logout needed. To undo:

```bash
sudo rm /etc/modules-load.d/termvisor-usbmon.conf /etc/udev/rules.d/70-termvisor-usbmon.rules
```

The same setup without the button: `pkexec termvisor --usbmon-setup`.

## Capturing

| Button | Captures |
|---|---|
| ● on a device | That device |
| >_ on a device | That device, in the serial view |
| Capture All Buses | Everything (`/dev/usbmon0`) |
| Open pcap… | A saved capture (no setup needed) |

The capture tab:

```
[ USB 1a86:55d2 USB Dual_Serial ● capturing 1520 events   Transfers ▾  All events  Find  Pause 💾 ⌫ ]
15:30:01.004500   3:6   ep 0  OUT CTRL S      7 B  CDC SET_LINE_CODING 115200 8N1
15:30:01.007500   3:6   ep 02 OUT BULK S      5 B  "help\r"
15:30:01.012000   3:6   ep 82 IN  BULK C     46 B  "U-Boot> help\r\nbootm - boot application image\r\n"
15:30:01.013500   3:6   ep 81 IN  INT  C      0 B STALL
```

Columns: time (µs), bus:device, endpoint and direction, type (CTRL,
BULK, INT, ISO), S submit / C complete / E error, length (`<64` is the
size asked for on an IN submit), status, meaning.

| View | Shows |
|---|---|
| Transfers | One line per transfer |
| Transfers + data | Plus a hex dump of each payload |
| Serial | The text of USB serial adapters: device to host green, host to device yellow |

- Hidden by default: submits and completions that carry nothing (no
  data, setup or error). "All events" shows them.
- Find filters lines; Pause stops the screen, capture continues.
- 💾 saves the kept events (last 100000) as pcap, link type 220, which
  Wireshark opens.
- "dropped" in the counter: the kernel buffer (16 MB) overflowed.

## Decoded

| Traffic | Shown as |
|---|---|
| Standard requests | `GET_DESCRIPTOR DEVICE len 18`, `SET_CONFIGURATION 1`, `CLEAR_FEATURE ENDPOINT_HALT ep 0x81` |
| Descriptors | Device (VID:PID, class), configuration (interfaces), strings |
| CDC-ACM | `SET_LINE_CODING 115200 8N1`, `SET_CONTROL_LINE_STATE DTR on RTS off`, data as text |
| FTDI | Requests by name; the two status bytes in each IN packet removed |
| CH34x, CP210x, PL2303 | Data as text |
| HID | Boot keyboard keys (`LShift+h`), mouse moves and buttons |
| Mass storage | `CBW … READ(10) lba 2048 blocks 8`, `CSW … ok` |
| Anything else | Text when printable, else the first bytes in hex |

Status words: STALL, cancelled, device gone, protocol error, CRC/timeout,
babble, short packet.

## Limits

usbmon sees what the host controller driver sends and receives. It
cannot see electrical or bit level events (NAK retries, CRC errors, SOF,
signal timing) or traffic that does not involve this computer; those
need a hardware analyzer. In a virtual machine it sees the devices passed
through to the guest.
