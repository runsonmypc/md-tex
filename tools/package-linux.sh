#!/bin/sh
# Builds the Linux release packages in target/linux: a tarball with the binary, the
# `.desktop` entry, the icon, the licenses and an install script, and an AppImage of the
# same binary. Needs THIRD-PARTY-LICENSES.md (tools/third-party-licenses.sh writes it).
# The AppImage step downloads a pinned appimagetool once into ~/.cache/md-tex-packaging.
#
# Both rely on the system's graphics and font libraries (Vulkan loader, X11, xkbcommon,
# fontconfig, FreeType), like any desktop app on the distribution.
set -eu
cd "$(dirname "$0")/.."
if [ ! -f THIRD-PARTY-LICENSES.md ]; then
    echo "THIRD-PARTY-LICENSES.md is missing; run tools/third-party-licenses.sh first" >&2
    exit 1
fi
cargo build --release -p mdt-app --locked
version=$(cargo pkgid -p mdt-app | sed 's/.*[#@]//')
arch=$(uname -m)
out=target/linux
name="md-tex-$version-$arch"
rm -rf "$out/$name" "$out/AppDir"
mkdir -p "$out/$name/bin" "$out/$name/share/applications" \
    "$out/$name/share/icons/hicolor/scalable/apps" "$out/$name/share/doc/md-tex"
cp target/release/md-tex "$out/$name/bin/md-tex"
cp assets/linux/md-tex.desktop "$out/$name/share/applications/"
cp assets/linux/md-tex.svg "$out/$name/share/icons/hicolor/scalable/apps/"
cp LICENSE THIRD-PARTY-LICENSES.md "$out/$name/share/doc/md-tex/"
cp assets/linux/install.sh "$out/$name/install.sh"
chmod +x "$out/$name/install.sh"
tar -C "$out" -czf "$out/$name.tar.gz" "$name"
echo "built $out/$name.tar.gz"

# AppImage: the same binary in an AppDir.
app="$out/AppDir"
mkdir -p "$app/usr/bin" "$app/usr/share/doc/md-tex"
cp target/release/md-tex "$app/usr/bin/md-tex"
cp LICENSE THIRD-PARTY-LICENSES.md "$app/usr/share/doc/md-tex/"
cp assets/linux/md-tex.desktop assets/linux/md-tex.svg "$app/"
printf '#!/bin/sh\nexec "$(dirname "$(readlink -f "$0")")/usr/bin/md-tex" "$@"\n' > "$app/AppRun"
chmod +x "$app/AppRun"
tool_version=1.9.1
case "$arch" in
    x86_64) tool_sha=ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0 ;;
    *) echo "no pinned appimagetool checksum for $arch" >&2; exit 1 ;;
esac
tool="$HOME/.cache/md-tex-packaging/appimagetool-$tool_version-$arch.AppImage"
if [ ! -x "$tool" ]; then
    mkdir -p "$(dirname "$tool")"
    curl -fsSL -o "$tool.part" \
        "https://github.com/AppImage/appimagetool/releases/download/$tool_version/appimagetool-$arch.AppImage"
    echo "$tool_sha  $tool.part" | sha256sum -c - >/dev/null
    chmod +x "$tool.part"
    mv "$tool.part" "$tool"
fi
ARCH="$arch" "$tool" --appimage-extract-and-run --no-appstream "$app" "$out/$name.AppImage" >/dev/null
echo "built $out/$name.AppImage"
