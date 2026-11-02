# Tools

Side panel → Tools. Each tool opens in a tab: controls on top, its log
below (the dock's Log page can write it to a file).

## TFTP server

For boot loaders, e.g. U-Boot `tftpboot`. Pick the folder, address,
port, and whether uploads are allowed (off by default). Supports the
`blksize`, `tsize` and `timeout` options and files larger than 32 MB.
Paths outside the folder are refused.

```
listening on 0.0.0.0:69
  U-Boot: setenv serverip 192.168.1.10; tftpboot ${loadaddr} <file>
192.168.1.50 ↓ sending zImage, 8.1 MB, 1468 B blocks
192.168.1.50 ✓ zImage 8.1 MB in 1.42 s (5.7 MB/s)
```

Port 69 is below 1024 and needs permission. Either:

- use port 6969 and tell U-Boot `setenv tftpdstp 6969`, or
- press "Allow Low Ports…" once: as administrator it runs
  `setcap cap_net_bind_service=+ep` on the termvisor program (restart
  termvisor after it; a rebuilt or updated program needs it again).

## HTTP server

Shares a folder: listings, downloads with resume (Range), and uploads when
allowed (browser form, or `curl -T file http://host:8000/dir/`). The log
shows each request with its status. URLs for this machine's addresses are
printed on start.

## Port scanner

| Field | Examples |
|---|---|
| Hosts | `10.0.0.5`, `board.local`, `192.168.1.0/24`, `10.0.0.10-50` (at most 4096 hosts) |
| Ports | `common` (ssh, http, modbus, mqtt, mavlink, …), `22,80,8000-8100` |
| Timeout | Per connection, ms |
| Banners | Read the first line an open port sends (SSH, FTP, SMTP versions) |

TCP connect scan, 256 at a time, no root needed. Only scan networks you
are allowed to.

## Ping

Runs the system `ping` (no root needed) and draws round trip times;
lost replies are red marks. The bar shows sent, loss, min, avg, max and
jitter (mean change between replies). Intervals below 0.2 s need root.

## Traceroute

Runs `tracepath -n` (no root needed) and shows its hops; hops that do
not answer are yellow.

## Wake-on-LAN

On the Tools page: name, MAC (`00:11:22:33:44:55`, `00-11-…` or
`001122…`) and optional broadcast address (default
255.255.255.255, UDP port 9). ▶ sends the magic packet.

## Settings file

`~/.config/termvisor/tools.toml` keeps the server folders, addresses,
ports and upload switches, and the Wake-on-LAN list:

```toml
[tftp]
dir = "/srv/tftp"
addr = "0.0.0.0"
port = 69
upload = false

[[wake]]
name = "lab-pc"
mac = "00:11:22:33:44:55"
broadcast = "192.168.1.255"
```
