# Recovery and Release All

What can stay behind, and what cleans it:

| Leftover | Cause | Cleaned by |
|---|---|---|
| Shell / ssh child | termvisor crashed | The kernel: the pty hangs up and the child gets SIGHUP |
| Open serial fd | termvisor crashed | The kernel closes it with the process |
| `/run/lock/LCK..<port>` | crash while a port was open | Startup, Release All, `--release-all` (only locks of dead PIDs) |
| ControlMaster socket `cm-*` | ssh master died | Same as above (only sockets nobody answers on) |
| Live shared SSH connection | normal, ends 60 s after its last tab | Release All, `--release-all` (`ssh -O exit`) |
| Hung termvisor holding a port | GUI froze | Watchdog dialog, Release All, `--release-all` |

## At startup

Stale locks and sockets are removed silently; a toast says what was removed.

## Release All (Ctrl+Shift+R, main menu)

Asks first and lists what it will do:

- closes the serial ports of this process; serial panes stay open and do
  not auto-reconnect until you press their reconnect button;
- closes shared SSH connections; SSH panes end and reconnect with Enter;
- removes stale locks and sockets;
- optionally stops other termvisor instances (checked by default when they
  hold a serial port): SIGTERM, then SIGKILL after 2 s.

## From a terminal

When termvisor will not start or its window is unusable:

```sh
termvisor --release-all
```

Example output:

```
stopping termvisor PID 4242 (/dev/ttyUSB0)
1 stale lock(s) removed, 1 shared SSH connection(s) closed, stopped PID 4242
```

Exit status is 1 if an instance could not be stopped.

## Watchdog

The GUI thread sends a heartbeat every second. If it stops for 5 s, a
separate helper process opens "termvisor is not responding" with:

- **Wait**: close the dialog; it comes back if the freeze continues;
- **Stop and Release**: stop the frozen instance and remove its stale
  locks and sockets.

The dialog closes by itself when the GUI recovers. A process paused with
SIGSTOP stops its watchdog too, so it is not detected; use
`--release-all` for that case.

## Scope

Only processes of the current user are inspected or stopped. Processes of
other programs (minicom, screen) are reported by name and PID when they
hold a port, but never stopped.
