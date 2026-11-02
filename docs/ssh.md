# SSH

SSH sessions run the system `ssh` (OpenSSH 8.4 or newer). Your
`~/.ssh/config`, agent, keys, ProxyJump and FIDO keys work as usual.

## Mosh

Session type `mosh` (or quick connect `mosh [user@]host`) runs the system
`mosh` with the session's ssh options passed as `--ssh="ssh -p … -l …"`.
Needs the `mosh` package locally and `mosh-server` on the host. Option
values with spaces or quotes are refused, because mosh splits that command
on spaces. Password and passphrase prompts use the same dialogs and keyring
as SSH; connection sharing is not used.

## Connection sharing

Every SSH tab adds:

```sh
-o ControlMaster=auto
-o ControlPath=$XDG_RUNTIME_DIR/termvisor/cm-%C
-o ControlPersist=60
```

- A second tab or split to the same host reuses the first login.
- The shared connection stays up 60 s after the last tab closes.
- The socket directory is created with mode 0700.
- Sharing is skipped when `XDG_RUNTIME_DIR` is unset or too long for a
  Unix socket path.

Inspect or close a shared connection by hand:

```sh
ssh -o ControlPath=$XDG_RUNTIME_DIR/termvisor/cm-%C -O check <host>
ssh -o ControlPath=$XDG_RUNTIME_DIR/termvisor/cm-%C -O exit <host>
```

## Prompts (askpass)

termvisor sets `SSH_ASKPASS` to itself with `SSH_ASKPASS_REQUIRE=force`,
so ssh asks through small dialogs instead of the terminal:

| ssh asks for | Dialog |
|---|---|
| Unknown host key | Fingerprint with Accept / Reject. OpenSSH still checks and writes `known_hosts`. |
| Password | Password field, "Remember in keyring" |
| Key passphrase | Same, stored per key file |
| Confirmation (e.g. agent confirm) | Yes / No |
| Notice (e.g. touch security key) | Message until ssh closes it |
| Anything else (OTP code) | Text field, never stored |

A **changed** host key is refused by OpenSSH. The pane then shows a red
"HOST KEY CHANGED" banner. Verify the new fingerprint out of band before
running `ssh-keygen -R <host>`.

## Keyring

Passwords and passphrases are stored in the system keyring (GNOME Keyring,
KWallet) through libsecret, never in termvisor files.

| Secret | Keyring id |
|---|---|
| Login password | `password:<target>` (e.g. `password:root@10.0.0.2:2222`) |
| Key passphrase | `key:<path to key>` |

- If a stored password is rejected, the next prompt shows the dialog
  instead of retrying the stored one; the new value replaces it.
- Session editor → **Remember Password** off: the keyring is never read or
  written for that session.
- Right-click an SSH session → **Forget Saved Password** removes it.

Self test against the real keyring:

```sh
cargo test -- --ignored keyring
```
