#!/bin/sh
# Build peekme.dev into site/public. Needs zola (ZOLA=path/to/zola to override).
set -eu
cd "$(dirname "$0")"
ZOLA="${ZOLA:-zola}"
{
  printf '+++\ntitle = "Changelog"\ndescription = "What changed in each peekme release."\nweight = 40\n+++\n\n'
  # The page has its own title, so drop the file's first heading.
  sed '1{/^# /d}' ../CHANGELOG.md
} > content/changelog.md
"$ZOLA" build "$@"
