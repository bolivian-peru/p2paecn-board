#!/usr/bin/env bash
# Makes the standalone partner pack archive: partner-pack/ and nothing else from this repository.
#
#   deploy/export-pack.sh [--draft] [--out <directory outside this repository>]
#
# Refuses while partner-pack/JOINING.md still holds the line "[OPERATOR: complete before sending]", unless --draft
# is given (a draft archive is named so, and is never sent to a partner). Packs only files git tracks under
# partner-pack/, as they are in the working tree, so a stray local file cannot ride along. Prints the SHA-256 of
# p2p.mjs and of the archive: send them by a channel other than the one that carries the archive.
set -euo pipefail

marker='[OPERATOR: complete before sending]'
draft=0
out=""
while [ $# -gt 0 ]; do
  case "$1" in
    --draft) draft=1 ;;
    --out) [ $# -ge 2 ] || { echo "export-pack: --out needs a directory" >&2; exit 1; }; out="$2"; shift ;;
    -h|--help) sed -n '2,9p' "$0"; exit 0 ;;
    *) echo "export-pack: unknown argument $1" >&2; exit 1 ;;
  esac
  shift
done

repo="$(cd "$(dirname "$0")/.." && pwd -P)"
pack="$repo/partner-pack"
[ -f "$pack/JOINING.md" ] && [ -f "$pack/p2p.mjs" ] || { echo "export-pack: no partner pack at $pack" >&2; exit 1; }

if grep -qF "$marker" "$pack/JOINING.md"; then
  if [ "$draft" -eq 0 ]; then
    echo "export-pack: partner-pack/JOINING.md still contains \"$marker\"." >&2
    echo "export-pack: write the operator's wording first (RUNBOOK section 10), or pass --draft for an archive not to be sent." >&2
    exit 1
  fi
  echo "export-pack: DRAFT: JOINING.md still contains the operator placeholder. Do not send this archive." >&2
fi

out="${out:-$PWD}"
mkdir -p "$out"
out="$(cd "$out" && pwd -P)"
case "$out/" in
  "$repo"/*) echo "export-pack: write the archive outside the repository (got $out)" >&2; exit 1 ;;
esac

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

# The files: tracked by git, under partner-pack/ only.
list="$(mktemp)"
stage="$(mktemp -d)"
trap 'rm -rf "$list" "$stage"' EXIT
git -C "$repo" ls-files -- partner-pack > "$list"
[ -s "$list" ] || { echo "export-pack: git lists no files under partner-pack/" >&2; exit 1; }
if grep -v '^partner-pack/' "$list" >/dev/null; then echo "export-pack: unexpected path in the file list" >&2; exit 1; fi
untracked="$(git -C "$repo" ls-files --others --exclude-standard -- partner-pack)"
[ -z "$untracked" ] || echo "export-pack: not packed (not tracked by git): $untracked" >&2
changed="$(git -C "$repo" status --porcelain -- partner-pack)"
[ -z "$changed" ] || echo "export-pack: note: partner-pack/ has uncommitted changes; the working tree is what is packed" >&2

# Copied as plain bytes into a staging directory: no extended attributes or other metadata from this machine.
while IFS= read -r f; do
  mkdir -p "$stage/$(dirname "$f")"
  cat "$repo/$f" > "$stage/$f"
  chmod 0644 "$stage/$f"
done < "$list"
find "$stage" -type d -exec chmod 0755 {} +

suffix=""; [ "$draft" -eq 1 ] && suffix="-DRAFT"
archive="$out/partner-pack-$(date -u +%Y%m%d)$suffix.tar.gz"
# No owner names or ids from this machine, and no macOS resource-fork (._*) entries.
if tar --version 2>/dev/null | grep -q 'GNU tar'; then
  (cd "$stage" && tar --sort=name --owner=0 --group=0 --numeric-owner -czf "$archive" -T "$list")
else
  (cd "$stage" && COPYFILE_DISABLE=1 tar --uid 0 --gid 0 --uname '' --gname '' --no-xattrs --no-mac-metadata -czf "$archive" -T "$list")
fi
chmod 0644 "$archive"

if tar -tzf "$archive" | grep -Ev '^partner-pack(/|$)' >/dev/null; then
  echo "export-pack: the archive holds a path outside partner-pack/; removed" >&2
  rm -f "$archive"; exit 1
fi

echo "archive        $archive ($(wc -l < "$list" | tr -d ' ') files)"
echo "sha256 p2p.mjs $(sha256 "$stage/partner-pack/p2p.mjs")"
echo "sha256 archive $(sha256 "$archive")"
