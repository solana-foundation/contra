#!/usr/bin/env bash
# install-prereqs.sh — install everything contra-deploy preflight checks for.
# Idempotent: re-runs are no-ops on already-satisfied prereqs.
# Targets Ubuntu/Debian. For other OSes, install manually per README.
set -euo pipefail

ANSIBLE_MIN=2.16
DOCKER_MIN=26

log()   { echo "[install-prereqs] $*"; }
ok()    { echo "  PASS  $*"; }
inst()  { echo "  INSTALL  $*"; }

# Compare two semver-ish versions: returns 0 if $1 >= $2.
ver_ge() {
  [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | head -n1)" = "$2" ]
}

# 1) Ansible >= ANSIBLE_MIN  ----------------------------------------------------
ansible_v="$(ansible --version 2>/dev/null | head -n1 | sed -nE 's/.*core ([0-9.]+).*/\1/p' || true)"
if [ -n "$ansible_v" ] && ver_ge "$ansible_v" "$ANSIBLE_MIN"; then
  ok "ansible-core $ansible_v"
else
  inst "ansible-core >= $ANSIBLE_MIN via pipx"
  if ! command -v pipx >/dev/null 2>&1; then
    sudo apt-get update -qq
    sudo apt-get install -y pipx
    pipx ensurepath
    export PATH="$HOME/.local/bin:$PATH"
  fi
  pipx install --force "ansible-core>=$ANSIBLE_MIN"
fi

# 2) community.general collection  ---------------------------------------------
if ansible-galaxy collection list community.general 2>/dev/null | grep -q community.general; then
  ok "community.general collection"
else
  inst "community.general collection"
  ansible-galaxy collection install community.general
fi

# 3) Docker Engine >= DOCKER_MIN  ----------------------------------------------
docker_v="$(docker version --format '{{.Server.Version}}' 2>/dev/null || true)"
if [ -n "$docker_v" ] && ver_ge "$docker_v" "$DOCKER_MIN"; then
  ok "docker $docker_v"
else
  inst "docker >= $DOCKER_MIN via get.docker.com"
  curl -fsSL https://get.docker.com | sh
  sudo usermod -aG docker "$USER" || true
  log "If 'docker' commands fail with 'permission denied', log out and back in (or run 'newgrp docker')."
fi

# 4) Docker Compose plugin  ----------------------------------------------------
if docker compose version >/dev/null 2>&1; then
  ok "docker compose plugin ($(docker compose version --short))"
else
  inst "docker-compose-plugin"
  sudo apt-get update -qq
  sudo apt-get install -y docker-compose-plugin
fi

log "All prerequisites satisfied."
log "Next: edit inventory.ini + vars/dev.yml + secrets.yml, then:"
log "      ansible-playbook deploy.yml -l dev"
