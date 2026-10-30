# Sessions

Saved sessions live in `~/.config/termvisor/sessions.toml` (mode 0600).
The Sessions panel shows, top to bottom:

1. **Favorites**: starred sessions from anywhere in the tree
2. **Recent**: last 10 sessions opened from the tree, finder or quick connect
3. **Local Shell** (built-in)
4. Entries from `sessions.toml`
5. **SSH Config**: hosts from `~/.ssh/config` (read-only, wildcards skipped)

Empty sections are hidden. The recent list is kept in
`~/.local/state/termvisor/recent.json` (or `$XDG_STATE_HOME`).

## Find Session (Ctrl+Shift+K)

Type part of a name; letters match in order, folder names count too
(`lab t1` finds "Lab / T153 board"). Up/Down select, Enter connects,
Esc closes. Plain Ctrl+K is left to the shell (readline kill-line).

Double-click or Enter opens a session. The refresh button reloads both files.

## Editing

- **+** button: new session or folder at the top level.
- Right-click a session: Connect, Edit, Duplicate, Add/Remove Favorite,
  New Session/Folder Here, Delete.
- Right-click a folder: Edit, New Session/Folder Here, Delete (asks first,
  shows how many items are inside).
- Right-click an SSH Config host: Connect, Copy to Sessions.
- Right-click empty space: New Session, New Folder.
- Drag a session or folder to move it:
  - onto a session: drops before or after it (upper or lower half);
  - onto a folder: middle half moves it inside, top or bottom quarter
    drops before or after the folder;
  - onto empty space: moves it to the end of the top level.
  A folder cannot be dropped into itself. Built-in rows cannot be dragged.

The editor has Name, Type (SSH or Shell), Color, Favorite and the connection
fields. Leave a field empty to inherit it from the parent folder. Enter in
any text field saves. Every change is written to `sessions.toml` at once.

## Format

```toml
version = 1

[[node]]
type = "folder"
name = "Lab"
color = "green"
defaults = { user = "root", jump = "gw", port = 2222 }

  [[node.children]]
  type = "ssh"
  name = "T153 board"
  host = "192.168.1.50"
  favorite = true

  [[node.children]]
  type = "ssh"
  name = "Build server"
  host = "10.0.0.9"
  user = "yk"
  port = 22
  key = "~/.ssh/id_ed25519"
  x11 = true
  args = ["-o", "ServerAliveInterval=30"]

[[node]]
type = "shell"
name = "Work"
dir = "~/Workspace"
```

## Types

| Type | Fields |
|---|---|
| `folder` | `name`, `color`, `defaults`, `children` |
| `ssh` | `name`, `host`, `port`, `user`, `key`, `jump`, `x11`, `args`, `color`, `favorite` |
| `shell` | `name`, `dir`, `color`, `favorite` |

| Field | Meaning |
|---|---|
| `color` | `blue`, `green`, `yellow`, `red`, `magenta`, `cyan` |
| `key` | Identity file (`ssh -i`), `~/` is expanded |
| `jump` | Jump host (`ssh -J`) |
| `x11` | X11 forwarding (`ssh -X`) |
| `args` | Extra ssh arguments, placed before the host |
| `favorite` | Star in the tree |

## Inheritance

A folder's `defaults` (`port`, `user`, `key`, `jump`, `x11`, `args`) apply to
every SSH session below it, through nested folders. A value set on the
session itself wins.

With the example above, "T153 board" runs:

```sh
ssh -p 2222 -l root -J gw -- 192.168.1.50
```

## Errors

- A host, user or jump value starting with `-` is rejected.
- A file that fails to parse is renamed to `sessions.toml.bad` and the
  tree starts empty; the original content is never overwritten.
