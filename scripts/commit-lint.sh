#!/usr/bin/env bash
# commit-lint.sh — valida mensajes de commit contra `.gitconventions`.
#
# El hook `.githooks/commit-msg` solo llama aquí: la lógica vive en un script que se
# puede ejecutar a mano (principio 5, Infraestructura).
#
# Uso:
#   scripts/commit-lint.sh .git/COMMIT_EDITMSG     valida un fichero de mensaje
#   scripts/commit-lint.sh -m "feat(core): ..."    valida un mensaje literal
#   scripts/commit-lint.sh --range origin/master..HEAD   valida un rango de commits
#   scripts/commit-lint.sh --last 5                valida los 5 últimos commits
#   git log -1 --format=%B | scripts/commit-lint.sh -    valida stdin
#
# Salida: 0 si todo pasa, 1 si hay algún error. Los avisos no cambian el código.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/conventions.sh
. "$SCRIPT_DIR/lib/conventions.sh"

TYPES="$(cfg_list types)"
SCOPES="$(cfg_list scopes)"
SCOPE_REQUIRED="$(cfg scope_required false)"
SUBJECT_MIN="$(cfg subject_min 10)"
SUBJECT_MAX="$(cfg subject_max 72)"
BODY_LINE_MAX="$(cfg body_line_max 100)"
FORBIDDEN="$(cfg_list forbidden_subjects)"
DISCOURAGED="$(cfg_list discouraged_prefixes)"
REQUIRE_BREAKING_FOOTER="$(cfg require_breaking_footer true)"

ERRORS=0

example() {
  hint "formato:  tipo(scope): asunto en imperativo"
  hint "ejemplo:  feat(core): add protocol version negotiation in hello"
  hint "tipos:    $TYPES"
  hint "scopes:   $SCOPES"
  hint "reglas:   docs/09-git.md · configuración: .gitconventions"
}

# Quita comentarios, la zona de diff de `--verbose` y las líneas en blanco del final.
clean_message() {
  if git stripspace --strip-comments >/dev/null 2>&1 </dev/null; then
    git stripspace --strip-comments
  else
    grep -v '^#'
  fi
}

# validate <mensaje> <etiqueta de origen>
validate() {
  local msg="$1" label="$2"
  local -a lines=()
  local line n_errors=0

  while IFS= read -r line; do lines+=("$line"); done <<< "$msg"

  local subject="${lines[0]-}"

  # Mensajes que git genera o que son temporales por diseño: no se validan aquí.
  # `fixup!` y `squash!` son legítimos en local; `pre-push` es quien no los deja salir.
  case "$subject" in
    "Merge "*|"Revert \""*|"fixup! "*|"squash! "*|"amend! "*)
      return 0 ;;
  esac

  if [ -z "${subject// }" ]; then
    err "$label: el mensaje está vacío"
    example
    ERRORS=$((ERRORS + 1))
    return 1
  fi

  # --- Cabecera ---
  local re='^([a-zA-Z]+)(\(([^)]*)\))?(!)?:[[:space:]](.+)$'
  if [[ ! "$subject" =~ $re ]]; then
    err "$label: la cabecera no sigue Conventional Commits"
    hint "recibido: $subject"
    example
    ERRORS=$((ERRORS + 1))
    return 1
  fi

  local type="${BASH_REMATCH[1]}"
  local has_scope_parens="${BASH_REMATCH[2]}"
  local scope="${BASH_REMATCH[3]}"
  local bang="${BASH_REMATCH[4]}"
  local text="${BASH_REMATCH[5]}"

  if ! in_list "$type" "$TYPES"; then
    err "$label: tipo «$type» no permitido"
    hint "tipos: $TYPES"
    n_errors=$((n_errors + 1))
  elif [ "$type" != "${type,,}" ]; then
    err "$label: el tipo va en minúscula («${type,,}», no «$type»)"
    n_errors=$((n_errors + 1))
  fi

  if [ -n "$has_scope_parens" ]; then
    if [ -z "$scope" ]; then
      err "$label: paréntesis de scope vacíos. Pon un scope o quita los paréntesis."
      n_errors=$((n_errors + 1))
    elif ! in_list "$scope" "$SCOPES"; then
      err "$label: scope «$scope» no está en .gitconventions"
      hint "scopes: $SCOPES"
      hint "si el scope es nuevo y legítimo, añádelo a .gitconventions en este mismo commit"
      n_errors=$((n_errors + 1))
    fi
  elif [ "$SCOPE_REQUIRED" = "true" ]; then
    err "$label: falta el scope (scope_required = true)"
    hint "scopes: $SCOPES"
    n_errors=$((n_errors + 1))
  fi

  # --- Asunto ---
  local len=${#text}
  if [ "$len" -gt "$SUBJECT_MAX" ]; then
    err "$label: el asunto tiene $len caracteres, máximo $SUBJECT_MAX"
    hint "lo que no cabe va en el cuerpo, no en la primera línea"
    n_errors=$((n_errors + 1))
  fi
  if [ "$len" -lt "$SUBJECT_MIN" ]; then
    err "$label: el asunto tiene $len caracteres, mínimo $SUBJECT_MIN"
    n_errors=$((n_errors + 1))
  fi

  case "$text" in
    *.) err "$label: el asunto no termina en punto"; n_errors=$((n_errors + 1)) ;;
  esac

  local first_word="${text%% *}"
  if [ "$first_word" = "$text" ]; then
    err "$label: el asunto es una sola palabra. Di qué hace el cambio."
    n_errors=$((n_errors + 1))
  fi

  local normalized
  normalized="$(printf '%s' "${text,,}" | sed -E 's/[[:punct:]]+$//')"
  if in_list "$normalized" "$FORBIDDEN"; then
    err "$label: «$text» no dice nada. Describe el cambio."
    n_errors=$((n_errors + 1))
  fi

  # Minúscula inicial, salvo acrónimos (ACP, RPC, UI, SQLite…).
  if [[ "$first_word" =~ ^[A-Z][a-záéíóúñ] ]]; then
    err "$label: el asunto empieza en minúscula («${first_word,}», no «$first_word»)"
    n_errors=$((n_errors + 1))
  fi

  # Imperativo presente: avisa, no bloquea.
  if in_list "${first_word,,}" "$DISCOURAGED"; then
    warn "$label: «$first_word» — el asunto va en imperativo presente (añade, corrige, mide)"
  fi

  # --- Cuerpo ---
  local total=${#lines[@]}
  if [ "$total" -gt 1 ] && [ -n "${lines[1]// }" ]; then
    err "$label: falta la línea en blanco entre el asunto y el cuerpo"
    n_errors=$((n_errors + 1))
  fi

  local i fence=0 body_line
  for ((i = 2; i < total; i++)); do
    body_line="${lines[i]}"
    case "$body_line" in
      '```'*) fence=$((1 - fence)); continue ;;
    esac
    [ "$fence" -eq 1 ] && continue
    # Las líneas sin espacios (URLs, rutas) y las tablas no se parten.
    case "$body_line" in
      *' '*) ;;
      *) continue ;;
    esac
    case "$body_line" in
      '|'*|'    '*) continue ;;
    esac
    if [ "${#body_line}" -gt "$BODY_LINE_MAX" ]; then
      err "$label: línea $((i + 1)) del cuerpo tiene ${#body_line} caracteres, máximo $BODY_LINE_MAX"
      n_errors=$((n_errors + 1))
    fi
  done

  # --- Cambios incompatibles ---
  if [ "$REQUIRE_BREAKING_FOOTER" = "true" ]; then
    local has_footer=0
    if printf '%s\n' "$msg" | grep -qE '^BREAKING[ -]CHANGE: .+'; then has_footer=1; fi
    if [ -n "$bang" ] && [ "$has_footer" -eq 0 ]; then
      err "$label: «$type!» sin footer BREAKING CHANGE"
      hint "añade una línea: BREAKING CHANGE: qué rompe y cómo se migra"
      n_errors=$((n_errors + 1))
    fi
    if [ -z "$bang" ] && [ "$has_footer" -eq 1 ]; then
      err "$label: hay footer BREAKING CHANGE pero falta el «!» en la cabecera"
      hint "cabecera: $type!${has_scope_parens}: … → el «!» va antes de los dos puntos"
      n_errors=$((n_errors + 1))
    fi
  fi

  if [ "$n_errors" -gt 0 ]; then
    example
    ERRORS=$((ERRORS + n_errors))
    return 1
  fi
  return 0
}

validate_commit_range() {
  local range="$1" sha count=0
  local shas
  shas="$(git rev-list --no-merges "$range" 2>/dev/null)" || {
    err "rango inválido: $range"
    exit 2
  }
  [ -z "$shas" ] && { ok "no hay commits nuevos que validar en $range"; return 0; }
  while IFS= read -r sha; do
    count=$((count + 1))
    validate "$(git log -1 --format=%B "$sha" | clean_message)" "$(git log -1 --format='%h %s' "$sha" | cut -c1-60)"
  done <<< "$shas"
  [ "$ERRORS" -eq 0 ] && ok "$count commit(s) con mensaje válido"
  return 0
}

main() {
  case "${1-}" in
    -m)
      [ $# -ge 2 ] || { err "falta el mensaje tras -m"; exit 2; }
      validate "$(printf '%s' "$2" | clean_message)" "mensaje"
      ;;
    --range)
      [ $# -ge 2 ] || { err "falta el rango tras --range"; exit 2; }
      validate_commit_range "$2"
      ;;
    --last)
      local n="${2:-1}"
      validate_commit_range "HEAD~${n}..HEAD"
      ;;
    -|"")
      validate "$(clean_message)" "mensaje"
      ;;
    -h|--help)
      sed -n '2,20p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      [ -f "$1" ] || { err "no existe el fichero: $1"; exit 2; }
      validate "$(clean_message < "$1")" "$(basename "$1")"
      ;;
  esac

  if [ "$ERRORS" -gt 0 ]; then
    head2 "$ERRORS problema(s). Commit rechazado."
    exit 1
  fi
  exit 0
}

main "$@"
