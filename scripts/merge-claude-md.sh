#!/bin/sh
# Installs or refreshes the "Anti-slop (always on)" section of CLAUDE.md without ever
# overwriting edits made to it.
#
#   scripts/merge-claude-md.sh           merge the block from scripts/claude-antislop.md
#   scripts/merge-claude-md.sh --check   report state, change nothing
#
# The managed block sits between the BEGIN/END markers. Its SHA-256 is stored in
# .claude-antislop.sha256 next to CLAUDE.md. If the block no longer matches that hash, it has
# been customised: the script leaves it alone and says so. Everything outside the markers is
# never touched.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
target="$root/CLAUDE.md"
source="$root/scripts/claude-antislop.md"
stamp="$root/.claude-antislop.sha256"
begin="<!-- BEGIN antislop (managed by scripts/merge-claude-md.sh) -->"
end="<!-- END antislop -->"

[ -f "$target" ] || { echo "CLAUDE.md not found at $target" >&2; exit 1; }
[ -f "$source" ] || { echo "block source not found at $source" >&2; exit 1; }

norm()       { sed -e 's/[[:space:]]*$//' -e '/^$/d'; }
block_hash() {
  awk -v b="$begin" -v e="$end" '$0 == b { on = 1; next } $0 == e { exit } on { print }' "$target" \
    | norm | shasum -a 256 | cut -d' ' -f1
}
new_hash()   { shasum -a 256 "$source" | cut -d' ' -f1; }
# The managed block is exactly the source file between the markers.
body_hash()  { norm < "$source" | shasum -a 256 | cut -d' ' -f1; }

if ! grep -qF "$begin" "$target"; then
  [ "${1:-}" = "--check" ] && { echo "block not installed"; exit 0; }
  { printf '\n%s\n' "$begin"; cat "$source"; printf '%s\n' "$end"; } >> "$target"
  block_hash > "$stamp"
  echo "installed the anti-slop block and recorded its hash"
  exit 0
fi

current=$(block_hash)
recorded=$(cat "$stamp" 2>/dev/null || echo "")
fresh=$(body_hash)

if [ "${1:-}" = "--check" ]; then
  if [ "$current" != "$recorded" ]; then
    echo "block has local edits (hash differs from $stamp); it will not be overwritten"
  elif [ "$current" = "$fresh" ]; then
    echo "block is current"
  else
    echo "block is outdated; run scripts/merge-claude-md.sh to update"
  fi
  exit 0
fi

if [ "$current" != "$recorded" ]; then
  echo "CLAUDE.md anti-slop block has local edits (hash $current != recorded $recorded)."
  echo "Leaving it untouched. To take the new version, delete the block between the markers and re-run."
  exit 0
fi

if [ "$current" = "$fresh" ]; then
  echo "block already current"
  exit 0
fi

tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT INT TERM
awk -v b="$begin" -v e="$end" -v src="$source" '
  $0 == b { print; while ((getline line < src) > 0) print line; skip = 1; next }
  $0 == e { skip = 0 }
  !skip { print }
' "$target" > "$tmp"
mv "$tmp" "$target"
trap - EXIT INT TERM
block_hash > "$stamp"
echo "updated the anti-slop block and recorded its hash"
