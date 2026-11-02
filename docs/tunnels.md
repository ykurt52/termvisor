# SSH tunnels

Port forwards without a terminal. Side panel → Tunnels.

| Type | ssh | Use |
|---|---|---|
| Local | `-L listen:dest` | A port here reaches `dest` through the server (database, web UI in a private network) |
| Remote | `-R listen:dest` | A port on the server reaches `dest` on this side |
| Dynamic SOCKS | `-D listen` | A SOCKS5 proxy here; apps that use it reach what the server reaches |

Fields: name, SSH host (`user@host[:port]` or an `~/.ssh/config` alias),
jump hosts (`ssh -J`, comma separated), listen (`8080` or
`127.0.0.1:8080`), destination (`host:port`), start with termvisor,
reconnect.

Each tunnel is its own `ssh -N` with `ExitOnForwardFailure=yes` and
keep-alives. It does not share the connection of terminal sessions, so
switching it off really closes its ports. Passwords and passphrases are
asked in termvisor's dialog, as for SSH sessions.

## Status

| Shown | Meaning |
|---|---|
| ● up · ≈ 1.2 MB moved | Running; bytes ssh read and wrote (from `/proc`, an estimate) |
| ◌ connecting | Started less than 3 s ago |
| ◌ reason; retrying in N s | Dropped; retries after 2, 5, 10, then 30 s |
| ○ reason | Ended and reconnect is off |

The reason is ssh's last error line, e.g. `bind [127.0.0.1]:8080:
Address already in use`. Release All (Ctrl+Shift+R) also stops every
tunnel. Tunnels end with termvisor, even if it crashes.

## File

`~/.config/termvisor/tunnels.toml`:

```toml
version = 1

[[tunnel]]
name = "db"
host = "admin@gw.example:2222"
jump = "bastion"
dir = "local"          # local, remote, dynamic
bind = "127.0.0.1:5433"
dest = "10.0.0.5:5432"
autostart = true
reconnect = true
```
