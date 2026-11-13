#!/bin/sh
# Builds target/release/md-tex.app. With --install, copies it to ~/Applications and puts
# an `md-tex` launcher on PATH (~/.local/bin) that runs the bundled binary, so windows
# opened from the terminal get the app icon and menu bar too.
# The license files go in Contents/Resources; THIRD-PARTY-LICENSES.md is included when
# tools/third-party-licenses.sh has written it (release builds always do).
set -eu
cd "$(dirname "$0")/.."
cargo build --release -p mdt-app --locked
app=target/release/md-tex.app
version=$(cargo pkgid -p mdt-app | sed 's/.*[#@]//')
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/md-tex "$app/Contents/MacOS/md-tex"
cp assets/macos/md-tex.icns "$app/Contents/Resources/md-tex.icns"
cp LICENSE "$app/Contents/Resources/"
if [ -f THIRD-PARTY-LICENSES.md ]; then
    cp THIRD-PARTY-LICENSES.md "$app/Contents/Resources/"
fi
sed "s/@VERSION@/$version/g" assets/macos/Info.plist > "$app/Contents/Info.plist"
codesign --force --sign - "$app"
echo "built $app"
if [ "${1:-}" = "--install" ]; then
    dest="$HOME/Applications/md-tex.app"
    mkdir -p "$HOME/Applications" "$HOME/.local/bin"
    rm -rf "$dest"
    cp -R "$app" "$dest"
    printf '#!/bin/sh\nexec "%s/Contents/MacOS/md-tex" "$@"\n' "$dest" > "$HOME/.local/bin/md-tex"
    chmod +x "$HOME/.local/bin/md-tex"
    # Register the document types with Launch Services.
    /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$dest"
    echo "installed $dest and $HOME/.local/bin/md-tex"
fi
