#!/bin/sh
# Prints the CHANGELOG.md section for the version given (the lines under "## <version>" up
# to the next "## "), and fails if there is none. The release workflow uses it for the notes.
set -eu
cd "$(dirname "$0")/.."
version=$1
notes=$(awk -v v="$version" '
    $0 ~ "^## " { if (found) exit; if ($2 == v) { found = 1; next } }
    found { print }
' CHANGELOG.md | sed '/./,$!d')
if [ -z "$(printf '%s' "$notes" | tr -d '[:space:]')" ]; then
    echo "CHANGELOG.md has no section for $version" >&2
    exit 1
fi
printf '%s\n' "$notes"
