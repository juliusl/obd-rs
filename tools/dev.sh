#!/usr/bin/env bash
# Reach the devcontainer from a host that cannot drive overlaybd devices itself.
#
# The Makefile sends every device target through `dev.sh exec` when it is not
# already on Linux, so this has to be the one place that knows how the container
# is created, restarted and repaired. Two things it takes care of that a plain
# `docker exec` does not:
#
#   1. A container started outside VS Code - `docker start`, a reboot of the
#      Docker host - skips postStartCommand and comes back with no configfs
#      mount and no overlaybd daemon. Every command is preceded by a check that
#      the running container was provisioned since it last started.
#   2. The devcontainer CLI derives the workspace path from the folder name
#      (/workspaces/<name>), so it is resolved from the bind mount rather than
#      assumed.
#
# Diagnostics go to stderr, so `dev.sh exec obdctl --json ...` still gives
# parseable stdout.
#
# Written for bash 3.2: macOS ships that as /bin/bash, and this script runs on
# the host rather than in the container.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# Matches `runArgs: ["--name", ...]` in .devcontainer/devcontainer.json.
CONTAINER="${OBD_DEV_CONTAINER:-obd-rs-dev}"
# Records the container start this workspace was provisioned for. Anything under
# / survives docker stop/start, so the value has to be compared rather than
# merely present.
MARKER="/run/obd-rs-provisioned"

info() { printf 'dev.sh: %s\n' "$1" >&2; }
die() {
  printf 'dev.sh: %s\n' "$1" >&2
  exit 1
}
have() { command -v "$1" >/dev/null 2>&1; }

usage() {
  cat >&2 <<'EOF'
usage: tools/dev.sh <command> [args]

  up            create or start the container, then make sure it is provisioned
  exec CMD...   run CMD in the container, bringing it up first
  shell         interactive login shell in the container
  stop          stop the container, keeping it and the cargo caches
  rebuild       recreate the container from .devcontainer
  status        report what exists and whether it can drive devices
EOF
  exit 2
}

require_docker() {
  have docker || die "docker is not on PATH. The devcontainer needs a Docker host; on macOS, colima start or Docker Desktop."
}

# missing, stopped or running.
container_state() {
  local status
  if ! status="$(docker inspect -f '{{.State.Status}}' "$CONTAINER" 2>/dev/null)"; then
    printf 'missing'
  elif [ "$status" = "running" ]; then
    printf 'running'
  else
    printf 'stopped'
  fi
}

# The container path of the workspace, read from whichever bind mount carries
# this repository.
workspace_dir() {
  local dir
  dir="$(docker inspect -f "{{range .Mounts}}{{if eq .Source \"$REPO_ROOT\"}}{{.Destination}}{{end}}{{end}}" "$CONTAINER" 2>/dev/null || true)"
  [ -n "$dir" ] || die "$CONTAINER has no bind mount for $REPO_ROOT. It was created for a different folder; tools/dev.sh rebuild recreates it for this one."
  printf '%s' "$dir"
}

started_at() { docker inspect -f '{{.State.StartedAt}}' "$CONTAINER"; }

mark_provisioned() {
  docker exec "$CONTAINER" bash -c 'printf "%s" "$2" > "$1"' _ "$MARKER" "$(started_at)"
}

provisioned() {
  local recorded
  recorded="$(docker exec "$CONTAINER" cat "$MARKER" 2>/dev/null || true)"
  [ "$recorded" = "$(started_at)" ]
}

ensure_provisioned() {
  if provisioned; then
    return 0
  fi
  info "provisioning $CONTAINER: kernel modules, configfs, overlaybd daemon"
  docker exec --workdir "$(workspace_dir)" "$CONTAINER" .devcontainer/provision.sh >&2
  mark_provisioned
}

cmd_up() {
  require_docker
  case "$(container_state)" in
  running) ;;
  stopped)
    info "starting $CONTAINER"
    docker start "$CONTAINER" >/dev/null
    ;;
  missing)
    have devcontainer || die "$CONTAINER does not exist yet and the devcontainer CLI is not on PATH. Create it from VS Code with Reopen in Container, or install the CLI: npm install -g @devcontainers/cli"
    info "creating $CONTAINER from .devcontainer; the first build takes a few minutes"
    devcontainer up --workspace-folder "$REPO_ROOT" >&2
    # `devcontainer up` has just run postStartCommand, which is provision.sh.
    mark_provisioned
    ;;
  esac
  ensure_provisioned
}

cmd_exec() {
  [ "$#" -gt 0 ] || die "exec needs a command to run"
  cmd_up
  # -i and -t only when there is something to attach them to: an interactive
  # terminal gets cargo's colors and progress, a pipe or a CI log gets neither
  # plus an undisturbed exit status.
  local flags
  flags=(--workdir "$(workspace_dir)")
  if [ -t 0 ]; then flags+=(-i); fi
  if [ -t 1 ]; then flags+=(-t); fi
  exec docker exec "${flags[@]}" "$CONTAINER" "$@"
}

cmd_shell() {
  cmd_up
  exec docker exec -it --workdir "$(workspace_dir)" "$CONTAINER" bash -l
}

cmd_stop() {
  require_docker
  case "$(container_state)" in
  missing) info "$CONTAINER does not exist" ;;
  stopped) info "$CONTAINER is already stopped" ;;
  running)
    docker stop "$CONTAINER" >/dev/null
    info "stopped $CONTAINER; the obd-rs-target and obd-rs-cargo-registry volumes are kept"
    ;;
  esac
}

cmd_rebuild() {
  require_docker
  have devcontainer || die "rebuilding needs the devcontainer CLI: npm install -g @devcontainers/cli. From VS Code, Rebuild Container does the same thing."
  info "recreating $CONTAINER from .devcontainer"
  # Docker's layer cache still applies, as it does behind VS Code's Rebuild
  # Container: an edited Dockerfile invalidates from that instruction on, and an
  # untouched one rebuilds in seconds.
  devcontainer up --workspace-folder "$REPO_ROOT" --remove-existing-container >&2
  mark_provisioned
}

cmd_status() {
  require_docker
  local state
  state="$(container_state)"
  printf 'container   %s (%s)\n' "$CONTAINER" "$state"
  if [ "$state" != "running" ]; then
    printf 'workspace   -\n'
    printf 'preflight   - (tools/dev.sh up)\n'
    return 0
  fi
  printf 'workspace   %s\n' "$(workspace_dir)"
  if provisioned; then
    printf 'provisioned yes, for the current start\n'
  else
    printf 'provisioned no, not since this container last started\n'
  fi
  if docker exec --workdir "$(workspace_dir)" "$CONTAINER" \
    test -x ./target/debug/obdctl 2>/dev/null; then
    docker exec --workdir "$(workspace_dir)" "$CONTAINER" \
      ./target/debug/obdctl preflight >&2 || true
  else
    printf 'preflight   - (obdctl is not built; make preflight)\n'
  fi
}

[ "$#" -gt 0 ] || usage
command="$1"
shift
case "$command" in
up) cmd_up "$@" ;;
exec) cmd_exec "$@" ;;
shell) cmd_shell "$@" ;;
stop) cmd_stop "$@" ;;
rebuild) cmd_rebuild "$@" ;;
status) cmd_status "$@" ;;
*) usage ;;
esac
