#!/usr/bin/env bash
# install-prereqs.sh — install everything private-channel-deploy preflight checks for.
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

# 3) Docker Engine + Compose plugin >= DOCKER_MIN  ----------------------------
# Install via Docker's official APT repo, with the GPG key fingerprint
# verified by apt on every subsequent update. This replaces the older
# `curl https://get.docker.com | sh` flow, which executed unverified remote
# code as root and ran the same risk every time the script was re-run.
# Both engine and compose plugin come from the same repo, so they're
# installed together in one apt invocation.
need_docker_install=false
docker_v="$(docker version --format '{{.Server.Version}}' 2>/dev/null || true)"
if [ -n "$docker_v" ] && ver_ge "$docker_v" "$DOCKER_MIN"; then
  ok "docker $docker_v"
else
  need_docker_install=true
fi
if docker compose version >/dev/null 2>&1; then
  ok "docker compose plugin ($(docker compose version --short))"
else
  need_docker_install=true
fi

if [ "$need_docker_install" = true ]; then
  inst "docker-ce + docker-compose-plugin >= $DOCKER_MIN via download.docker.com APT repo"

  # Distro detection: only Ubuntu/Debian are supported by this script.
  if ! [ -r /etc/os-release ]; then
    log "ERROR: cannot read /etc/os-release; install Docker manually per README."
    exit 1
  fi
  # shellcheck disable=SC1091
  . /etc/os-release
  case "${ID:-}" in
    ubuntu|debian) ;;
    *) log "ERROR: unsupported distro '${ID:-?}'. Install Docker manually per README."; exit 1 ;;
  esac

  sudo apt-get update -qq
  sudo apt-get install -y ca-certificates curl gnupg

  # Pin Docker's GPG key under /etc/apt/keyrings/. Subsequent `apt-get update`
  # invocations verify every package signature against this key, so the
  # integrity of every install is enforced by the package manager rather
  # than by trusting whatever bytes a `curl | sh` happens to receive.
  sudo install -m 0755 -d /etc/apt/keyrings
  curl -fsSL "https://download.docker.com/linux/${ID}/gpg" \
    | sudo gpg --dearmor --yes -o /etc/apt/keyrings/docker.gpg
  sudo chmod a+r /etc/apt/keyrings/docker.gpg

  arch="$(dpkg --print-architecture)"
  codename="${VERSION_CODENAME:-$(lsb_release -cs 2>/dev/null || true)}"
  if [ -z "$codename" ]; then
    log "ERROR: cannot determine distro codename for the APT source line."
    exit 1
  fi

  echo "deb [arch=${arch} signed-by=/etc/apt/keyrings/docker.gpg] https://download.docker.com/linux/${ID} ${codename} stable" \
    | sudo tee /etc/apt/sources.list.d/docker.list >/dev/null

  sudo apt-get update -qq
  sudo apt-get install -y docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin

  sudo usermod -aG docker "$USER" || true
  log "If 'docker' commands fail with 'permission denied', log out and back in (or run 'newgrp docker')."
fi

log "All prerequisites satisfied."
log "Next: edit inventory.ini + vars/dev.yml + secrets.yml, then:"
log "      ansible-playbook deploy.yml -l dev"
