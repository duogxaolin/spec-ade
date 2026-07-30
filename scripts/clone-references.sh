#!/usr/bin/env bash
#
# Restore the study archive that the repository deliberately does not track:
# `docs/references/*` (~600M of third-party sources) and `docs/spec-ade-clone`
# (the upstream docs this project reverse-engineers).
#
# The whole `docs/` tree is gitignored — see .gitignore. Nothing here is a build
# dependency of `src/`, so the project compiles and tests fine without any of it;
# this only matters alongside the private design notes that cite these sources.
# No code is copied from them; each keeps its own license (README lists them).
#
# Usage:
#   scripts/clone-references.sh            # clone whatever is missing
#   scripts/clone-references.sh zed ttyd   # only these
#   SHALLOW=0 scripts/clone-references.sh  # full history (default: depth 1)
#
# Already-present directories are left untouched; this never deletes anything.

set -euo pipefail

# Repo root, regardless of where the script is invoked from.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Depth 1 by default: these are read-only references and the full histories are
# large (wezterm and zed alone are ~360M checked out).
if [[ "${SHALLOW:-1}" == "1" ]]; then
  DEPTH=(--depth 1)
else
  DEPTH=()
fi

# name|destination|url|branch
# Branches are pinned to what upstream actually uses; `master` vs `main` differs
# per repo, so it is recorded rather than guessed.
REPOS=(
  "spec-ade-clone|docs/spec-ade-clone|https://github.com/duogxaolin/spec-ade-clone.git|"
  "agent-client-protocol|docs/references/agent-client-protocol|https://github.com/zed-industries/agent-client-protocol.git|main"
  "bottom|docs/references/bottom|https://github.com/ClementTsang/bottom.git|main"
  "esbuild|docs/references/esbuild|https://github.com/evanw/esbuild.git|main"
  "gitbutler|docs/references/gitbutler|https://github.com/gitbutlerapp/gitbutler.git|master"
  "NotepadAI|docs/references/NotepadAI|https://github.com/nullmastermind/NotepadAI.git|master"
  "ripgrep|docs/references/ripgrep|https://github.com/BurntSushi/ripgrep.git|master"
  "sshx|docs/references/sshx|https://github.com/ekzhang/sshx.git|main"
  "tokio-cron-scheduler|docs/references/tokio-cron-scheduler|https://github.com/mvniekerk/tokio-cron-scheduler.git|main"
  "ttyd|docs/references/ttyd|https://github.com/tsl0922/ttyd.git|main"
  "wezterm|docs/references/wezterm|https://github.com/wez/wezterm.git|main"
  "zed|docs/references/zed|https://github.com/zed-industries/zed.git|main"
)

WANTED=("$@")

want() {
  [[ ${#WANTED[@]} -eq 0 ]] && return 0
  local n
  for n in "${WANTED[@]}"; do [[ "$n" == "$1" ]] && return 0; done
  return 1
}

cloned=0 skipped=0 failed=0

for entry in "${REPOS[@]}"; do
  IFS='|' read -r name dest url branch <<<"$entry"
  want "$name" || continue

  target="$ROOT/$dest"
  if [[ -e "$target/.git" ]]; then
    printf '  skip   %-24s already present\n' "$name"
    skipped=$((skipped + 1))
    continue
  fi
  if [[ -e "$target" ]] && [[ -n "$(ls -A "$target" 2>/dev/null)" ]]; then
    # Non-empty but not a git checkout: refuse rather than clobber local work.
    printf '  SKIP   %-24s exists and is not a git checkout\n' "$name"
    skipped=$((skipped + 1))
    continue
  fi

  printf '  clone  %-24s %s\n' "$name" "$url"
  args=("${DEPTH[@]}")
  [[ -n "$branch" ]] && args+=(--branch "$branch")
  # Keep going on failure: one unreachable repo should not abort the rest.
  if git clone --quiet "${args[@]}" "$url" "$target"; then
    cloned=$((cloned + 1))
  else
    printf '  FAIL   %-24s clone failed\n' "$name"
    failed=$((failed + 1))
  fi
done

printf '\n%d cloned, %d skipped, %d failed\n' "$cloned" "$skipped" "$failed"
[[ $failed -eq 0 ]]
