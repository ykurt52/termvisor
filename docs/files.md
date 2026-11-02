# Files (SFTP) and transfers

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

- Four transfers run at the same time; each worker reuses one SFTP session
  per host.
- Folders are copied recursively and merge into an existing folder of the
  same name.
- Downloads are written to `<name>.part` and renamed when complete; a
  cancelled or failed download leaves no partial file.
- Downloaded files keep the remote permission bits; uploads keep the local
  ones.
- **If target exists**: *Keep both* (default, writes `name (1).ext`),
  *Overwrite*, or *Skip* (skipped files are counted on the row).
- **Clear Finished** removes done and failed rows.
- The Files panel refreshes when an upload to its host finishes.

## Not yet (milestone 7b)

FTP/FTPS, editing a remote file in a local editor, following the
terminal's directory, resuming interrupted transfers, dual-pane view.

## Tests

The SFTP client and the transfer queue are tested against the system
OpenSSH `sftp-server` (`/usr/lib/openssh/sftp-server` or
`/usr/lib/ssh/sftp-server`), without a network connection:

```sh
cargo test sftp transfers
```
