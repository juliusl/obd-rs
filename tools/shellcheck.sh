#!/usr/bin/env bash
# Shellcheck every shell script in the repository.
#
# The set is discovered rather than listed so a new script cannot slip past
# lint, and it includes files git does not track yet: a script is most likely to
# be wrong before its first commit.
#
# The binary is taken from PATH where it exists - the devcontainer image
# installs it - and from its official image otherwise, which is what makes this
# work on a macOS host with nothing installed.
#
# Written for bash 3.2, the /bin/bash on macOS.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
IMAGE="koalaman/shellcheck:stable"

cd "$REPO_ROOT"

scripts=()
while IFS= read -r file; do
  case "$file" in
  *.sh)
    scripts+=("$file")
    continue
    ;;
  esac
  # Package maintainer scripts are named by the packaging format - lib/deb
  # holds postinst and postrm - so extensions cannot find them and the shebang
  # is what identifies them.
  [ -r "$file" ] || continue
  case "$(head -n 1 "$file" 2>/dev/null)" in
  '#!'*sh) scripts+=("$file") ;;
  esac
done < <(git ls-files --cached --others --exclude-standard | sort)

if [ "${#scripts[@]}" -eq 0 ]; then
  echo "shellcheck.sh: no shell scripts found under $REPO_ROOT" >&2
  exit 1
fi

if command -v shellcheck >/dev/null 2>&1; then
  exec shellcheck "${scripts[@]}"
fi

if command -v docker >/dev/null 2>&1; then
  echo "shellcheck.sh: no shellcheck on PATH, running $IMAGE" >&2
  exec docker run --rm --volume "$REPO_ROOT:/mnt" --workdir /mnt "$IMAGE" "${scripts[@]}"
fi

echo "shellcheck.sh: needs either shellcheck on PATH (brew install shellcheck, apt install shellcheck) or docker to run $IMAGE" >&2
exit 1
