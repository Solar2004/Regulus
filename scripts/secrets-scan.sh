#!/usr/bin/env bash
# secrets-scan.sh — busca credenciales en lo que está a punto de entrar en git.
#
# Uso:
#   scripts/secrets-scan.sh              ficheros en el índice (lo que hace pre-commit)
#   scripts/secrets-scan.sh --all        todos los ficheros versionados
#   scripts/secrets-scan.sh ruta ...     ficheros concretos
#
# Patrones: `.gitsecrets` si existe (una expresión regular extendida por línea), y si no
# los de aquí abajo. Para marcar un falso positivo, pon `regulus:allow-secret` en la línea.
#
# Esto no sustituye a rotar una clave filtrada: un secreto commiteado está comprometido
# aunque borres el commit.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/conventions.sh
. "$SCRIPT_DIR/lib/conventions.sh"

PATTERNS_FILE="$REPO_ROOT/.gitsecrets"
DEFAULT_PATTERNS=(
  'sk-ant-[A-Za-z0-9_-]{20,}'
  'sk-[A-Za-z0-9]{32,}'
  'gh[pousr]_[A-Za-z0-9]{30,}'
  'xox[baprs]-[0-9A-Za-z-]{10,}'
  'AKIA[0-9A-Z]{16}'
  'AIza[0-9A-Za-z_-]{30,}'
  '-----BEGIN [A-Z ]*PRIVATE KEY-----'
  '(api[_-]?key|apikey|secret|password|passwd|token|auth)[[:space:]]*[:=][[:space:]]*["'"'"'][^"'"'"']{12,}'
  'postgres(ql)?://[^:]+:[^@]+@'
  'mysql://[^:]+:[^@]+@'
)

load_patterns() {
  if [ -f "$PATTERNS_FILE" ]; then
    mapfile -t PATTERNS < <(grep -vE '^[[:space:]]*(#|$)' "$PATTERNS_FILE")
  else
    PATTERNS=("${DEFAULT_PATTERNS[@]}")
  fi
}

files_in_index() {
  git diff --cached --name-only --diff-filter=ACMR
}

all_tracked() {
  git ls-files
}

# Contenido a inspeccionar: la versión del índice si existe ahí, el fichero si no.
content_of() {
  local f="$1"
  if git ls-files --error-unmatch "$f" >/dev/null 2>&1 || git diff --cached --name-only -- "$f" | grep -q .; then
    git show ":$f" 2>/dev/null || cat "$f" 2>/dev/null
  else
    cat "$f" 2>/dev/null
  fi
}

scan() {
  local found=0 f pat line
  for f in "$@"; do
    [ -f "$f" ] || continue
    # Binarios y el propio fichero de patrones no se escanean.
    case "$f" in
      .gitsecrets|scripts/secrets-scan.sh) continue ;;
    esac
    # Los binarios no se escanean: el ruido es garantizado y la señal nula.
    grep -Iq . "$f" 2>/dev/null || continue
    local body
    body="$(content_of "$f")" || continue
    for pat in "${PATTERNS[@]}"; do
      while IFS= read -r line; do
        [ -z "$line" ] && continue
        case "$line" in *regulus:allow-secret*) continue ;; esac
        err "posible secreto en $f:${line%%:*}"
        hint "${line#*:}"
        found=$((found + 1))
      done < <(printf '%s\n' "$body" | grep -inE -e "$pat" | cut -c1-200)
    done
  done
  return "$found"
}

load_patterns

declare -a TARGETS=()
case "${1-}" in
  --all) mapfile -t TARGETS < <(all_tracked) ;;
  "")    mapfile -t TARGETS < <(files_in_index) ;;
  -h|--help) sed -n '2,14p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
  *)     TARGETS=("$@") ;;
esac

[ "${#TARGETS[@]}" -eq 0 ] && exit 0

scan "${TARGETS[@]}"
found=$?
if [ "$found" -eq 0 ]; then
  exit 0
fi

head2 "$found posible(s) credencial(es). Commit abortado."
hint "falso positivo → añade «regulus:allow-secret» a la línea, o ajusta .gitsecrets"
exit 1
