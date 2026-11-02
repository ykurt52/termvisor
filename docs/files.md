# Files (SFTP, FTP) and transfers

## Files panel

Activity bar → Files. The panel follows the last focused **SSH tab** and
browses that host over SFTP. It connects when the panel is first shown,
through the tab's shared connection (`ssh … -s -- <host> sftp`), so there is
no second login. Focusing an SSH tab on another host switches the panel; a
local shell or serial tab leaves it on the last host.

```
root@10.0.0.2
[↑] [⌂] [⟳] [+folder] [upload] [download] [hidden]
/home/root
Name            Size     Modified           Mode
📁 bin                   2026-11-02 09:43   drwxr-xr-x
📄 app.log      12 kB    2026-11-02 09:40   -rw-r--r--
12 items
```

| Action | How |
|---|---|
| Open a folder | Double-click, Enter, or type a path and press Enter |
| Download | Double-click a file (to `~/Downloads`), toolbar button, or right-click → Download / Download To… |
| Upload | Drag files or folders from the file manager onto the list, toolbar button, or right-click → Upload Files… / Upload Folder… |
| New folder, rename, permissions, delete | Right-click menu (permissions take an octal mode like `755`) |
| Copy remote path | Right-click → Copy Path |
| Hidden files | Eye button |
| Reconnect | Refresh button when the panel says "not connected" |

Delete removes folders with everything inside, after a confirmation.

## Transfers (bottom dock)

Every upload or download is a row with direction, progress, size, speed
and a cancel button.

- Four transfers run at the same time; each worker reuses one session
  per server (SFTP or FTP).
- Folders are copied recursively and merge into an existing folder of the
  same name.
- Downloads and uploads go through `<name>.part`, renamed when complete
  (see Resume and retry).
- Downloaded files keep the remote permission bits; uploads keep the local
  ones.
- **If target exists**: *Keep both* (default, writes `name (1).ext`),
  *Overwrite*, or *Skip* (skipped files are counted on the row).
- **Clear Finished** removes done and failed rows.
- The Files panel refreshes when an upload to its host finishes.

## FTP and FTPS

FTP servers open in the Files panel (no terminal tab):

- Quick connect: `ftp://host`, `ftp://user@host:2121`, `ftps://user@nas`.
- Start page → FTP, or a saved session of type `ftp`.
- Empty user means anonymous. Otherwise the password comes from the keyring
  or a dialog with "Remember in keyring" (id `password:ftp://user@host:21`).
- Encryption: none, or explicit TLS (FTPES, `AUTH TLS`). Implicit FTPS on
  port 990 is not supported.
- "Accept Any Certificate" allows self-signed device certificates.
- Passive mode is on by default; turn it off for active mode.
- Listing uses `MLSD`, falling back to `LIST` (Unix and DOS formats).
- Permissions use `SITE CHMOD`; uploads do not set a mode.

```toml
[[node]]
type = "ftp"
name = "NAS"
host = "nas.lan"
port = 21
user = "yk"
tls = "explicit"     # none, explicit
passive = true
insecure = false
```

## Resume and retry

Downloads write `<name>.part` locally, uploads write `<name>.part` on the
server; the file gets its real name only when complete. If a transfer
fails (network drop, host reboot) the `.part` stays and the row shows
**Retry**: it continues from the size already transferred. Cancel removes
the `.part`.

## Edit in local editor

Right-click a file → **Edit in Local Editor**. The file is downloaded to a
private folder (`$XDG_RUNTIME_DIR/termvisor/edit/…`, mode 0700) and opened
with the default app for its type. Every save is uploaded back (700 ms
after the last write, overwriting the remote file). The upload shows in
Transfers like any other.

## Follow the terminal

With the location button on (default), the panel goes to the folder the
SSH tab's shell is in, for the host the panel shows. The remote shell must
report its directory with OSC 7. For bash on the remote host:

```sh
# ~/.bashrc on the remote host
PROMPT_COMMAND='printf "\e]7;file://%s%s\e\\" "$HOSTNAME" "$PWD"'
```

Many distributions already do this through `/etc/profile.d/vte.sh` when it
is sourced.

## Not yet

Dual-pane view (local and remote side by side).

## Tests

The SFTP client, resume and the transfer queue are tested against the
system OpenSSH `sftp-server` (`/usr/lib/openssh/sftp-server` or
`/usr/lib/ssh/sftp-server`), without a network connection. FTP has unit
tests for the listing parsers, plus a live test against any FTP server:

```sh
TV_FTP_TEST="127.0.0.1:2121:user:pass:0" cargo test -- --ignored live_ftp   # last field 1 = FTPES
```

Unit tests:

```sh
cargo test sftp
cargo test transfers
cargo test ftp
```
