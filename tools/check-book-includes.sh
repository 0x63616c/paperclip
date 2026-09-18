#!/usr/bin/env bash
# mdBook 0.4's `{{#include}}` preprocessor logs a missing target as a warning
# and still exits 0 (verified locally, WWW-18) — a moved ADR or a renamed
# anchor would silently render as blank prose instead of failing CI. This is
# the check that actually gates it: every `{{#include <path>[:anchor]}}` in
# book/src must resolve to a real file.
set -euo pipefail

book_src="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/book/src"
status=0

while IFS= read -r -d '' file; do
  dir="$(dirname "$file")"
  while IFS= read -r target; do
    resolved="$dir/$target"
    if [[ ! -f "$resolved" ]]; then
      echo "missing include target: $file -> $target" >&2
      status=1
    fi
  done < <(grep -oE '\{\{#include[[:space:]]+[^:}[:space:]]+' "$file" | sed -E 's/^\{\{#include[[:space:]]+//')
done < <(find "$book_src" -name '*.md' -print0)

if [[ "$status" -eq 0 ]]; then
  echo "all {{#include}} targets resolve"
fi
exit "$status"
