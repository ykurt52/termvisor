# Windows port

VTE runs only on Unix, so termvisor gets its own terminal view
(`src/vt.rs`) that works wherever GTK runs. `alacritty_terminal` keeps
the screen state (escape sequences, scrollback, selection); the view draws
it with Pango and turns keys and mouse into input.

## Phases

| Phase | Work | State |
|---|---|---|
| 1 | Terminal view next to VTE, compared side by side | prototype |
| 2 | Replace `vte::Terminal` everywhere with the view | |
| 3 | Platform layer: ConPTY, Windows Credential Manager, Unix-only parts behind `cfg(unix)` | |
| 4 | Windows build in CI, zip with the GTK runtime | |

## Comparing with VTE

```bash
cargo run -- --vt-demo
```

Opens one window: VTE on the left, the new view on the right, each
running `$SHELL`. Font comes from the settings; colors are a fixed
palette, the same on both sides.

## The view

| Area | Done |
|---|---|
| Drawing | 16/256/true colors, bold, italic, underline, strike, dim, inverse, hidden, wide and combining characters, box drawing and block characters drawn to fill the cell |
| Cursor | block, beam, underline; hollow when not focused |
| Keys | text through the input method (dead keys, compose), arrows, Home/End, PgUp/PgDn, F1-F12, Ctrl and Alt combinations, application cursor mode; Ctrl+Shift is left to the application |
| Mouse | select (double click: word, triple: line, Alt: block), primary selection, middle click paste, wheel scrolls the scrollback (arrow keys in full-screen programs) |
| Other | Shift+PgUp/PgDn scroll, Shift+Insert paste, bracketed paste, focus reports, title, OSC 52 copy, color and size queries |

Not yet: mouse reporting to programs (vim, htop, mc mouse), scrollbar,
search, hyperlinks, cursor blink, bell.

API names match VTE's (`feed`, `connect_commit`, `column_count`,
`row_count`, `set_font`, `set_font_scale`, `set_colors`,
`set_color_cursor`, `set_color_highlight`, `set_scrollback_lines`,
`reset`, `text`, `copy_clipboard`, `paste_clipboard`) so phase 2 is mostly
a type change.
