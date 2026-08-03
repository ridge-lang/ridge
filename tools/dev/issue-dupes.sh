#!/usr/bin/env bash
# issue-dupes.sh — search existing Ridge issues before opening a new one.
#
# Usage:
#   tools/dev/issue-dupes.sh <keyword> [keyword...]
#
# Searches open AND closed issues/PRs in ridge-lang/ridge for the given
# keywords (OR-ed), so you can spot duplicates before filing. Prints the
# top matches with state, labels, and title. Exit code is always 0; this
# is a reading aid, not a gate.
#
# Requires the GitHub CLI (`gh`) authenticated for github.com.
set -euo pipefail

if [ "$#" -eq 0 ]; then
  echo "usage: $0 <keyword> [keyword...]" >&2
  exit 64
fi

# Build an OR query: (kw1 OR kw2 OR kw3)
query="("
first=1
for kw in "$@"; do
  if [ "$first" -eq 1 ]; then
    query+="$kw"
    first=0
  else
    query+=" OR $kw"
  fi
done
query+=")"

echo "== Searching ridge-lang/ridge issues for: $query =="
echo

gh issue list \
  --repo ridge-lang/ridge \
  --search "$query" \
  --state all \
  --limit 20 \
  --json number,state,title,labels \
  --template '{{range .}}{{"\t"}}#{{.number}}{{"\t"}}{{.state}}{{"\t"}}{{range .labels}}{{.name}} {{end}}{{"\t"}}{{.title}}{{"\n"}}{{end}}' \
  | column -t -s $'\t' 2>/dev/null || true

echo
echo "If nothing relevant shows up, go ahead and file:"
echo "  https://github.com/ridge-lang/ridge/issues/new/choose"
