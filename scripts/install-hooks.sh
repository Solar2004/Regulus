#!/usr/bin/env bash
# install-hooks.sh — enchufa el harness de git de Regulus a este clon.
#
#   scripts/install-hooks.sh              instala
#   scripts/install-hooks.sh --uninstall  lo quita (deja el repo como estaba)
#   scripts/install-hooks.sh --check      dice si está instalado; 1 si no
#
# Idempotente: ejecutarlo dos veces no hace nada raro. Los hooks no se versionan en
# `.git/hooks` (git no lo permite): se apunta `core.hooksPath` a `.githooks/`, que sí está
# en el repo. Cada clon nuevo necesita correr esto una vez — es el único paso manual.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/conventions.sh
. "$SCRIPT_DIR/lib/conventions.sh"

HOOKS_DIR=".githooks"

require_git_version() {
  local have want="2.9"
  have="$(git --version | sed -E 's/[^0-9]*([0-9]+\.[0-9]+).*/\1/')"
  if [ "$(printf '%s\n%s\n' "$want" "$have" | sort -V | head -1)" != "$want" ]; then
    err "git $have es demasiado viejo: core.hooksPath necesita git >= $want"
    exit 1
  fi
}

do_install() {
  require_git_version
  git -C "$REPO_ROOT" config core.hooksPath "$HOOKS_DIR"
  git -C "$REPO_ROOT" config commit.template ".gitmessage"
  chmod +x "$REPO_ROOT/$HOOKS_DIR"/* "$REPO_ROOT/scripts"/*.sh 2>/dev/null || true

  ok "hooks instalados (core.hooksPath = $HOOKS_DIR)"
  ok "plantilla de mensaje instalada (.gitmessage)"
  head2 "Qué corre ahora, y cuándo"
  hint "commit-msg  → valida el mensaje contra .gitconventions"
  hint "pre-commit  → secretos, conflictos a medio resolver, ficheros enormes, fmt/clippy si hay Rust"
  hint "pre-push    → revalida todos los mensajes del rango, bloquea fixup! y rewrite de master"
  head2 "A mano, sin commitear"
  hint "scripts/commit-lint.sh -m 'feat(core): añade negociación de protocolo'"
  hint "scripts/commit-lint.sh --last 5"
  hint "scripts/secrets-scan.sh --all"
  head2 "Las reglas"
  hint "humanos y agentes: docs/09-git.md · configuración: .gitconventions"
}

do_uninstall() {
  git -C "$REPO_ROOT" config --unset core.hooksPath 2>/dev/null || true
  git -C "$REPO_ROOT" config --unset commit.template 2>/dev/null || true
  ok "hooks desinstalados. Los ficheros siguen en $HOOKS_DIR; solo está desconectado."
}

do_check() {
  local current
  current="$(git -C "$REPO_ROOT" config --get core.hooksPath || true)"
  if [ "$current" = "$HOOKS_DIR" ]; then
    ok "harness activo (core.hooksPath = $current)"
    exit 0
  fi
  err "harness NO instalado. Corre: scripts/install-hooks.sh"
  exit 1
}

case "${1-}" in
  --uninstall) do_uninstall ;;
  --check)     do_check ;;
  -h|--help)   sed -n '2,11p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' ;;
  "")          do_install ;;
  *)           err "opción desconocida: $1"; exit 2 ;;
esac
