#!/usr/bin/env bash
# Install the "Sluice Bandwidth" GNOME Shell extension for the current user.
#
# Copies the extension into ~/.local/share/gnome-shell/extensions/<uuid>/, compiles its GSettings
# schema, and enables it. On Wayland you must log out and back in for the shell to load a NEW
# extension (a reload isn't enough); on X11, Alt+F2 → `r` restarts the shell.
set -euo pipefail

UUID="sluice-bandwidth@preston-peterson.github.io"
SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEST="${XDG_DATA_HOME:-$HOME/.local/share}/gnome-shell/extensions/$UUID"

command -v glib-compile-schemas >/dev/null 2>&1 || {
    echo "glib-compile-schemas not found (install libglib2.0-dev-bin or glib2.0-devel)." >&2
    exit 1
}

echo "Installing $UUID → $DEST"
rm -rf "$DEST"
mkdir -p "$DEST/schemas"
install -m 0644 "$SRC/metadata.json" "$SRC/extension.js" "$SRC/prefs.js" "$SRC/stylesheet.css" "$DEST/"
install -m 0644 "$SRC/schemas/"*.gschema.xml "$DEST/schemas/"
glib-compile-schemas "$DEST/schemas"
echo "  ✓ installed"

if gnome-extensions enable "$UUID" 2>/dev/null; then
    echo "  ✓ enabled"
else
    echo "  ! could not enable automatically — enable it in the Extensions app, or run:"
    echo "      gnome-extensions enable $UUID"
fi

echo
echo "On Wayland, log out and back in for GNOME Shell to load the new extension."
echo "Then open its settings with:  gnome-extensions prefs $UUID"
