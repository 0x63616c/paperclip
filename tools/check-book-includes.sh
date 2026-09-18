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

# The mirror of the check above, and the one that was missing. The loop so far
# proves every page the book *has* points at a real ADR; it says nothing about
# an ADR the book has no page for. WWW-81 added ADR-0039 and no book page, the
# README's index linked it anyway, and `mdbook build` failed on a dangling link
# — after CI had already gone green on the commit that introduced it. Adding an
# ADR and forgetting its page is the ordinary mistake here, so gate it.
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
for adr in "$repo_root"/docs/adr/[0-9]*.md; do
  slug="$(basename "$adr" .md)"
  if [[ ! -f "$book_src/decisions/$slug.md" ]]; then
    echo "ADR with no book page: docs/adr/$slug.md -> book/src/decisions/$slug.md" >&2
    status=1
  fi
  if ! grep -qF "decisions/$slug.md" "$book_src/SUMMARY.md"; then
    echo "ADR missing from book/src/SUMMARY.md: $slug" >&2
    status=1
  fi
done

if [[ "$status" -eq 0 ]]; then
  echo "all {{#include}} targets resolve, and every ADR has a book page"
fi
exit "$status"
