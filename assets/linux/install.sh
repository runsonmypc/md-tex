#!/bin/sh
# Installs md-tex for the current user: the binary in ~/.local/bin, the desktop entry and
# icon under ~/.local/share, so file managers offer md-tex for Markdown files.
# PREFIX=/usr/local (as root) installs system-wide instead.
# From a source checkout: assets/linux/install.sh target/release/md-tex
set -eu
here=$(dirname "$(readlink -f "$0")")
prefix=${PREFIX:-$HOME/.local}
if [ "$#" -gt 0 ]; then
    binary=$1
    desktop="$here/md-tex.desktop"
    icon="$here/md-tex.svg"
else
    binary="$here/bin/md-tex"
    desktop="$here/share/applications/md-tex.desktop"
    icon="$here/share/icons/hicolor/scalable/apps/md-tex.svg"
    docs="$here/share/doc/md-tex"
fi
mkdir -p "$prefix/bin" "$prefix/share/applications" "$prefix/share/icons/hicolor/scalable/apps"
cp "$binary" "$prefix/bin/md-tex"
# An absolute Exec line works even when the bin directory is not on the session's PATH.
sed "s|^Exec=md-tex|Exec=$prefix/bin/md-tex|" "$desktop" \
    > "$prefix/share/applications/md-tex.desktop"
cp "$icon" "$prefix/share/icons/hicolor/scalable/apps/"
# The release tarball carries the licenses; a source checkout has them at its root.
if [ -n "${docs:-}" ]; then
    mkdir -p "$prefix/share/doc/md-tex"
    cp "$docs"/* "$prefix/share/doc/md-tex/"
fi
update-desktop-database "$prefix/share/applications" 2>/dev/null || true
gtk-update-icon-cache -q "$prefix/share/icons/hicolor" 2>/dev/null || true
echo "installed md-tex in $prefix"
echo "To make it the default for Markdown: xdg-mime default md-tex.desktop text/markdown"
