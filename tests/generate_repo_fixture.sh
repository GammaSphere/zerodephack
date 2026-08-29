#!/usr/bin/env bash
# Build the repository fixture that tests/repository.rs reads.
#
# Run once at authoring time; the result is committed, so `cargo test` needs
# nothing but cargo. git is a development-time tool here and is disclosed in
# STDLIB.md - strata itself never invokes it.
#
#     bash tests/generate_repo_fixture.sh
#
# The object database is stored as `dot_git` rather than `.git`, because git
# will not track a nested `.git` directory as ordinary files - it would record a
# gitlink instead. The test opens it directly with Repository::open.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEST="$HERE/fixtures/repo"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

cd "$WORK"
git init -q -b main .
git config commit.gpgsign false

author() { export GIT_AUTHOR_NAME="$1" GIT_AUTHOR_EMAIL="$2" GIT_COMMITTER_NAME="$1" GIT_COMMITTER_EMAIL="$2"; }
at() { export GIT_AUTHOR_DATE="$1 +0000" GIT_COMMITTER_DATE="$1 +0000"; }

# --- a root commit, and a file large enough that later edits become deltas ---
author "Ada Lovelace" "ada@example.com"
at "2024-01-01T00:00:00"
mkdir -p src docs
python3 -c "
lines = ['line %d: the quick brown fox jumps over the lazy dog' % i for i in range(3000)]
open('src/engine.py','w').write('\n'.join(lines))
"
echo "# Fixture" > README.md
git add -A && git commit -qm "initial commit"

# --- many small edits to one file, which is what forces delta chains ---
for round in 1 2 3 4 5 6 7 8; do
  at "2024-01-0$((round + 1))T00:00:00"
  python3 -c "
import random
random.seed($round)
lines = open('src/engine.py').read().split('\n')
for _ in range(20):
    lines[random.randrange(len(lines))] = 'line edited in round $round'
open('src/engine.py','w').write('\n'.join(lines))
"
  git add -A && git commit -qm "engine: round $round"
done

# --- a second author, so ownership and bus factor have something to divide ---
author "Grace Hopper" "grace@example.com"
at "2024-02-01T00:00:00"
echo "def parse(): pass" > src/parser.py
echo "engine and parser move together" >> docs/design.md
git add -A && git commit -qm "add the parser"

at "2024-02-02T00:00:00"
echo "def parse(text): return text" > src/parser.py
echo "one line" >> src/engine.py
git add -A && git commit -qm "parser and engine, together again"

at "2024-02-03T00:00:00"
echo "def parse(text): return text.strip()" > src/parser.py
echo "another line" >> src/engine.py
git add -A && git commit -qm "parser and engine, once more"

# --- a file with a non-UTF-8-friendly name, and a binary blob ---
at "2024-02-04T00:00:00"
printf 'caf\xc3\xa9\n' > "docs/café.md"
printf '\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDRbinary' > docs/logo.png
git add -A && git commit -qm "add unicode and binary files"

# --- an exact rename, which must collapse to one change event ---
at "2024-02-05T00:00:00"
git mv src/parser.py src/parsing.py
git commit -qm "rename parser to parsing"

# --- a deletion ---
at "2024-02-06T00:00:00"
git rm -q docs/logo.png
git commit -qm "drop the logo"

# --- a merge commit, which the default walk must skip ---
author "Ada Lovelace" "ada@example.com"
at "2024-03-01T00:00:00"
git checkout -q -b side
echo "side work" > docs/side.md
git add -A && git commit -qm "work on a branch"
git checkout -q main
at "2024-03-02T00:00:00"
echo "main work" > docs/main.md
git add -A && git commit -qm "work on main"
at "2024-03-03T00:00:00"
git merge -q --no-ff side -m "merge side into main"

# --- an empty commit, which touches nothing ---
at "2024-03-04T00:00:00"
git commit -q --allow-empty -m "an empty commit"

# --- pack it, so the fixture exercises packfiles and deltas, not loose objects
git repack -a -d -q --depth=50 --window=250
git gc -q --prune=now 2>/dev/null || true

rm -rf "$DEST"
mkdir -p "$DEST"
cp -r .git "$DEST/dot_git"
# Reflogs and hooks are noise and machine-specific; drop them.
rm -rf "$DEST/dot_git/logs" "$DEST/dot_git/hooks" "$DEST/dot_git/index"

echo "fixture written to $DEST/dot_git"
echo "commits: $(git rev-list --count --all)"
echo "objects: $(git count-objects -v | grep in-pack)"
