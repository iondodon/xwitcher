# xwitcher

xwitcher is a lightweight Alt+Tab replacement for X11 written in Rust. It grabs the classic `Alt+Tab` key sequence, shows a custom overlay listing open windows, and lets you pick a target while keeping your most recently used applications at the top. The project focuses on being self-contained, fast, and visually clean while still showing window icons and titles sourced from EWMH metadata or your desktop icon themes.

## Features

- Tracks windows by most recently used order, so repeated `Alt+Tab` oscillates between your last two apps instantly.
- Displays window titles alongside icons pulled from `_NET_WM_ICON`, icon themes, or matching `.desktop` files.
- Works even when some EWMH hints are missing by falling back to other X11 properties or icon directories.
- Draws its own override-redirect overlay window, avoiding interference from compositors or window managers.
- Handles `Alt+Tab`, `Alt+Shift+Tab`, and `Alt+Escape` without hard-coding keycodes, instead mapping the real keys from the X11 server at runtime.

## Requirements

- X11/Xorg session (Wayland requires XWayland support).
- A Rust toolchain (edition 2024) via [rustup](https://rustup.rs/).
- Development headers for X11 if your distribution splits them (e.g. `libx11-dev` on Debian-based systems).

## Building

```bash
cargo build --release
```

The optimized binary will be at `target/release/xwitcher`.

## Running

Launch the binary inside your X session:

```bash
target/release/xwitcher &
```

The process grabs the keyboard when `Alt`+`Tab` is pressed and keeps running in the background. Press `Ctrl+C` in the launching terminal or kill the process to stop it.

## Keyboard behaviour

- `Alt+Tab`: cycle forward through the window list.
- `Alt+Shift+Tab`: cycle backward.
- Releasing `Alt`: focus the currently highlighted entry.
- `Alt+Escape`: cancel and keep the original focus.

Because xwitcher registers its own grabs, make sure your window manager or desktop environment does not already intercept the same shortcuts.

## Icon lookup

If a window does not expose an icon via `_NET_WM_ICON`, xwitcher searches standard icon directories such as `~/.icons`, `~/.local/share/icons`, the `XDG_DATA_DIRS` locations, and `/usr/share/pixmaps`. It also parses `.desktop` files in common application directories to map application names or startup classes to icon files. Ensure your icon themes are installed system-wide or per-user if you rely on this fallback.

## Limitations

- Only X11 is supported; running under a pure Wayland compositor is out of scope.
- There is no configuration layer yet—behaviour is currently hard-coded aside from the runtime key lookups.
- Some compositors might still apply focus rules that override the requested target window.
