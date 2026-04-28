#!/bin/sh
# lefthook rustfmt helper — avoids inline-shell quoting issues on Windows.
# Called by lefthook.yml with {staged_files} as arguments.

for f in "$@"; do
  [ ! -f "$f" ] && continue

  dir=$(dirname "$f")
  while [ "$dir" != "." ] && [ "$dir" != "/" ] && [ ! -f "$dir/Cargo.toml" ]; do
    dir=$(dirname "$dir")
  done

  edition=$(grep -m1 '^edition' "$dir/Cargo.toml" 2>/dev/null | sed 's/.*"\([^"]*\)".*/\1/')
  rustfmt --edition "${edition:-2024}" "$f"
done
