# Android (adb)

Shell, logcat and files of Android devices, through the `adb` command
line tool. Install it first: Debian/Ubuntu `adb`, Arch `android-tools`.
The device needs USB debugging on (or wireless debugging after
`adb connect`).

## Android page

Activity bar → phone icon.

```
Pixel 7        [>_] [≡] [📁]
  2B111FDH2000
FAKE002        (buttons off)
  FAKE002 · allow USB debugging on the device
```

| Button | Opens |
|---|---|
| >_ | `adb shell` in a new tab |
| ≡ | Logcat view in a new tab |
| 📁 | The device in the Files panel |

- The list follows `adb track-devices -l`: plugging or unplugging a device
  updates it at once.
- adb runs only after this page is opened the first time; starting
  termvisor does not start the adb server.
- Unauthorized: accept the RSA key prompt on the device.

Quick connect: `adb://<serial>`, or `adb://` when one device is attached.

## Shell

Runs `adb -s <serial> wait-for-device shell` on termvisor's pty, so
timestamps, highlight, logging and command buttons work as in any shell.
After an unplug the session ends; Enter waits for the device and
reconnects.

## Logcat

```
[ logcat Pixel 7 ● streaming 1520 of 20000 lines   Info ▾ │Tags│PID or package│Find│ Pause 🗑 ⌫ ]
11-02 14:30:01.300  4242  4250 W ExampleApp: slow frame: 48 ms
```

| Control | Effect |
|---|---|
| Level | Lowest level shown: Verbose … Fatal |
| Tags | Parts of tag names, comma separated (`ActivityManager, MyApp`) |
| PID or package | Process ids or package names; names are looked up with `pidof` and again every 3 s, so an app restart is followed |
| Find | Text anywhere in the line |
| Pause | Stops the screen; lines are still kept |
| 🗑 | `logcat -c` on the device (asks first) |
| ⌫ | Clears the screen and the kept lines |

- Colors: V gray, D cyan, I green, W yellow, E red, F bold red.
- The last 20000 lines are kept; filter changes apply to them too.
- Logging (dock → Log) writes every line unfiltered.
- When the device goes away, logcat waits for it and resumes from the
  last line's time, dropping repeated lines.

## Files

The Files panel works as for SFTP: browse, upload/download with the
transfer queue, resume, rename, delete, chmod, edit in a local editor.
It starts in `/sdcard`.

| Operation | Command on the device |
|---|---|
| List | `find <dir>/ -mindepth 1 -maxdepth 1 -exec stat -c … {} +` |
| Download | `adb exec-out cat` (resume: `tail -c +N`) |
| Upload | `adb exec-in "cat > file"` (resume: `>>`), then `chmod` |
| Other | `mkdir`, `rmdir`, `rm`, `mv`, `chmod` |

Needs Android 7 or newer (toybox `stat`, `find`, `exec-in`). Folders the
shell user cannot read (e.g. `/data`) show an error.

## Settings

`~/.config/termvisor/settings.json`:

```json
{ "adb": "adb" }
```

A full path picks a specific adb (e.g. from the Android SDK
`platform-tools`).

## Testing without a device

`tests/fixtures/fake-adb` stands in for adb: devices FAKE001 (ready) and
FAKE002 (unauthorized), FAKE003 appears after 2 s; files live under
`$FAKE_ADB_ROOT`; every call is appended to `$FAKE_ADB_LOG`. The unit
tests use it by full path and never run the real adb.

To try the UI against it, with a separate config so the real adb is
never used:

```bash
mkdir -p /tmp/fake-adb-root/sdcard /tmp/tv-cfg/termvisor
echo "{\"adb\": \"$PWD/tests/fixtures/fake-adb\"}" > /tmp/tv-cfg/termvisor/settings.json
FAKE_ADB_ROOT=/tmp/fake-adb-root FAKE_ADB_LOG=/tmp/fake-adb.log XDG_CONFIG_HOME=/tmp/tv-cfg cargo run
```
