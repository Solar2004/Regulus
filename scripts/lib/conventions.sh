#!/usr/bin/env bash
# conventions.sh — lector de `.gitconventions` y utilidades de salida.
#
# No se ejecuta solo: lo cargan los scripts de `scripts/` y los hooks de `.githooks/`.
# Existe para que las reglas vivan en un único sitio (regla 5 de AGENTS.md) y para que
# ningún hook contenga un literal que debería ser configuración (principio 1).

# shellcheck shell=bash

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
CONV_FILE="${REGULUS_GITCONVENTIONS:-$REPO_ROOT/.gitconventions}"

# Devuelve todas las líneas de valor de una clave (una clave puede repetirse: los
# valores se acumulan). Quita comentarios y espacios de sobra.
_cfg_raw() {
  [ -f "$CONV_FILE" ] || return 0
  sed -E 's/[[:space:]]*#.*$//' "$CONV_FILE" \
    | grep -E "^[[:space:]]*$1[[:space:]]*=" \
    | sed -E "s/^[[:space:]]*$1[[:space:]]*=[[:space:]]*//" \
    | sed -E 's/[[:space:]]+$//' \
    | grep -v '^$'
}

# cfg <clave> <valor-por-defecto> — valor escalar (la primera aparición gana).
cfg() {
  local v
  v="$(_cfg_raw "$1" | head -1)"
  if [ -n "$v" ]; then printf '%s' "$v"; else printf '%s' "${2-}"; fi
}

# cfg_list <clave> — todos los valores en una línea, separados por espacios.
cfg_list() {
  _cfg_raw "$1" | tr '\n' ' ' | sed -E 's/[[:space:]]+/ /g; s/^ //; s/ $//'
}

# in_list <aguja> <lista separada por espacios>
in_list() {
  local needle="$1" item
  for item in $2; do
    [ "$needle" = "$item" ] && return 0
  done
  return 1
}

# --- Salida ----------------------------------------------------------------------
# Color solo si hay terminal: los hooks también corren desde GUIs y CI.
if [ -t 2 ] && [ -z "${NO_COLOR:-}" ]; then
  C_RED=$'\033[31m'; C_YEL=$'\033[33m'; C_GRN=$'\033[32m'
  C_DIM=$'\033[2m'; C_BLD=$'\033[1m'; C_OFF=$'\033[0m'
else
  C_RED=''; C_YEL=''; C_GRN=''; C_DIM=''; C_BLD=''; C_OFF=''
fi

err()  { printf '%s✖%s %s\n' "$C_RED" "$C_OFF" "$1" >&2; }
warn() { printf '%s⚠%s %s\n' "$C_YEL" "$C_OFF" "$1" >&2; }
ok()   { printf '%s✔%s %s\n' "$C_GRN" "$C_OFF" "$1" >&2; }
hint() { printf '%s  %s%s\n' "$C_DIM" "$1" "$C_OFF" >&2; }
head2() { printf '\n%s%s%s\n' "$C_BLD" "$1" "$C_OFF" >&2; }

# Avisa si un hook tarda más que el presupuesto: un hook lento se acaba saltando con
# --no-verify, y un hook que se salta no existe (principio 3, Rápido).
timer_start() { HOOK_T0=$(date +%s); }
timer_check() {
  local budget elapsed
  budget="$(cfg hook_time_budget 10)"
  [ -n "${HOOK_T0:-}" ] || return 0
  elapsed=$(( $(date +%s) - HOOK_T0 ))
  if [ "$elapsed" -gt "$budget" ]; then
    warn "el hook $1 tardó ${elapsed}s (presupuesto: ${budget}s). Recórtalo o sube hook_time_budget en .gitconventions."
  fi
}
