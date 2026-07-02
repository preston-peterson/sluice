# Sluice Bandwidth (GNOME Shell extension)

A small top-bar network throughput indicator for GNOME Shell. It shows live download/upload
rates, with a click-menu to toggle it, switch units, pick the interface, open full settings, and
open or quit Sluice.

It is **standalone**: it reads `/proc/net/dev` directly (~1 Hz, diffing the byte counters), so it
needs **no root, makes no network calls, and does not require the Sluice app or engine to be
running**. It lives in the Sluice repo for convenience but is an independent GNOME extension.

## Two display modes

- **Text** — `↓ 1.2 MB/s   ↑ 340 KB/s`.
- **Graph** — a compact up/down sparkline (upload above the midline, download below) with small
  `↑`/`↓` rate labels. Wider in the bar, but a nice at-a-glance view.

Colour convention throughout: **↓ green = download, ↑ blue = upload**.

## Settings

Via the indicator menu or `gnome-extensions prefs sluice-bandwidth@preston-peterson.github.io`:
display mode, units (bytes/bits), show download and/or upload, graph width, network interface
(Automatic combines all non-virtual interfaces, or pick one), and refresh interval.

## Install

```bash
extension/install.sh
```

This copies the extension into `~/.local/share/gnome-shell/extensions/`, compiles the schema, and
enables it. **On Wayland you must log out and back in** for GNOME Shell to load a newly-installed
extension (on X11, Alt+F2 → `r`).

Total host throughput only — per-application byte accounting isn't available to an unprivileged
reader of `/proc/net/dev`.

Targets GNOME Shell 45–50.
