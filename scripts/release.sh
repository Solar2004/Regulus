#!/usr/bin/env bash
# release.sh — builds a release of Regulus into <out>/<version>/ (docs/06-stack.md §5.4).
#
#   scripts/release.sh                       CLI/TUI program + desktop app, channel stable
#   scripts/release.sh --cli-only            only the `regulus` program
#   scripts/release.sh --channel beta        the channel written in the manifest
#   scripts/release.sh --out /tmp/rel        instead of ./dist
#   scripts/release.sh --sign-key k.key      sign with this key (or $REGULUS_SIGNING_KEY)
#   scripts/release.sh --url https://x/rel   base URL for the app's latest.json
#                                            (or $REGULUS_UPDATE_URL)
#   scripts/release.sh --bundles appimage    app bundles to build (default appimage,deb)
#
# The version is the workspace's (`Cargo.toml`, [workspace.package]); nothing else holds
# it. Publishing is copying <out>/<version>/* to <url>/<channel>/.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TAURI_DIR="$ROOT/crates/regulus-tauri"
FRONTEND_DIR="$TAURI_DIR/frontend"
# System libraries the desktop app links on Linux (Tauri 2 prerequisites).
APP_LIBS="${REGULUS_APP_LIBS:-webkit2gtk-4.1 javascriptcoregtk-4.1 libsoup-3.0 gtk+-3.0 librsvg-2.0 openssl}"

channel="stable"
out="$ROOT/dist"
cli_only=0
sign_key=""
url="${REGULUS_UPDATE_URL:-}"
bundles="appimage,deb"

usage() { sed -n '2,15p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }
say() { printf 'release: %s\n' "$*" >&2; }
die() { say "error: $*"; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --cli-only) cli_only=1 ;;
    --channel) channel="${2:?--channel needs a value}"; shift ;;
    --out) out="${2:?--out needs a directory}"; shift ;;
    --sign-key) sign_key="${2:?--sign-key needs a file}"; shift ;;
    --url) url="${2:?--url needs a base URL}"; shift ;;
    --bundles) bundles="${2:?--bundles needs a list}"; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown argument: $1 (see --help)" ;;
  esac
  shift
done

[[ "$channel" =~ ^[A-Za-z0-9._-]+$ && "$channel" != "." && "$channel" != ".." ]] \
  || die "channel '$channel': use letters, digits, '.', '-' or '_'"
url="${url%/}"
case "$out" in /*) ;; *) out="$PWD/$out" ;; esac

# One source: cargo resolves version.workspace and the target dir of .cargo/config.toml.
metadata="$(cd "$ROOT" && cargo metadata --no-deps --format-version 1)"
field() { printf '%s' "$metadata" | grep -o "\"$1\":\"[^\"]*\"" | head -1 | cut -d'"' -f4; }
target_dir="$(field target_directory)"
version="$(sed -n '/^\[workspace.package\]/,/^\[/{s/^version *= *"\(.*\)"/\1/p}' "$ROOT/Cargo.toml")"
[ -n "$version" ] || die "no version under [workspace.package] in Cargo.toml"

os="$(uname -s | tr '[:upper:]' '[:lower:]')"
arch="$(uname -m)"
case "$os" in darwin) os="macos" ;; esac
case "$arch" in arm64) arch="aarch64" ;; amd64) arch="x86_64" ;; esac
platform="$os-$arch"   # as Rust names it: std::env::consts::{OS, ARCH}

case "$out" in ""|/) die "--out must be a directory, not '$out'" ;; esac
[ -n "$version" ] || die "no version in Cargo.toml"
dest="$out/$version"
rm -rf "$dest"
mkdir -p "$dest"
started=$(date +%s)
failed=""

# --- the regulus program (core, CLI and TUI) ---------------------------------------
say "building regulus $version for $platform (cargo build --release -p regulus-cli)"
t0=$(date +%s)
(cd "$ROOT" && cargo build --release -p regulus-cli)
cli_secs=$(( $(date +%s) - t0 ))
bin="$target_dir/release/regulus"
said="$("$bin" --version)"
[ "$said" = "regulus $version" ] || die "'regulus --version' says '$said', the workspace says $version"
cli_name="regulus-$platform"
cp "$bin" "$dest/$cli_name"
entries=("$cli_name|$platform|cli")

# --- the desktop app ---------------------------------------------------------------
app_secs=0
app_files=()
if [ "$cli_only" = 0 ]; then
  missing=()
  if [ "$os" = "linux" ]; then
    for lib in $APP_LIBS; do pkg-config --exists "$lib" 2>/dev/null || missing+=("$lib"); done
  fi
  command -v pnpm >/dev/null || missing+=("pnpm")
  if [ ${#missing[@]} -gt 0 ]; then
    say "app skipped: missing ${missing[*]} (install them, or pass --cli-only)"
    failed="app (missing ${missing[*]})"
  else
    say "building the app (tauri build --no-bundle, then tauri bundle: $bundles)"
    t0=$(date +%s)
    [ -d "$FRONTEND_DIR/node_modules" ] || (cd "$FRONTEND_DIR" && pnpm install --frozen-lockfile)
    # From the Tauri crate, as `pnpm tauri:build` does: the CLI looks for src-tauri below.
    # Compile once, then one `tauri bundle` per type, so a bundle that fails (AppImage
    # downloads linuxdeploy) does not take the others with it.
    # NO_STRIP: linuxdeploy's own strip fails on binaries from newer toolchains.
    tauri="$FRONTEND_DIR/node_modules/.bin/tauri"
    if (cd "$TAURI_DIR" && "$tauri" build --no-bundle); then
      bundle_dir="$target_dir/release/bundle"
      for kind in ${bundles//,/ }; do
        if ! (cd "$TAURI_DIR" && NO_STRIP="${NO_STRIP:-true}" "$tauri" bundle --bundles "$kind"); then
          failed="${failed:+$failed, }app $kind (tauri bundle failed)"
        fi
      done
      while IFS= read -r file; do
        name="$(basename "$file")"
        cp "$file" "$dest/$name"
        case "$name" in
          *.AppImage) kind="appimage" ;;
          *.deb) kind="deb" ;;
          *.rpm) kind="rpm" ;;
          *.dmg) kind="dmg" ;;
          *) kind="app" ;;
        esac
        entries+=("$name|$platform-$kind|app")
        app_files+=("$name")
      done < <(find "$bundle_dir" -maxdepth 2 -type f -newer "$dest" \( -name "*_${version}_*.AppImage" -o -name "*_${version}_*.deb" -o -name "*-${version}-*.rpm" -o -name "*_${version}_*.dmg" \) 2>/dev/null)
      [ ${#app_files[@]} -gt 0 ] || failed="${failed:-app (tauri made no bundle for $version)}"
    else
      failed="app (tauri build failed)"
    fi
    app_secs=$(( $(date +%s) - t0 ))
  fi
fi

# --- checksums and manifest ---------------------------------------------------------
cd "$dest"
names=()
for entry in "${entries[@]}"; do names+=("${entry%%|*}"); done
sha256sum "${names[@]}" > SHA256SUMS
date_utc="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
{
  printf '{\n  "version": "%s",\n  "date": "%s",\n  "channel": "%s",\n  "files": [' "$version" "$date_utc" "$channel"
  sep=""
  for entry in "${entries[@]}"; do
    IFS='|' read -r name plat kind <<<"$entry"
    sum="$(grep "  $name\$" SHA256SUMS | cut -d' ' -f1)"
    size="$(stat -c %s "$name" 2>/dev/null || stat -f %z "$name")"
    printf '%s\n    {"name": "%s", "sha256": "%s", "size": %s, "platform": "%s", "kind": "%s"}' \
      "$sep" "$name" "$sum" "$size" "$plat" "$kind"
    sep=","
  done
  printf '\n  ]\n}\n'
} > manifest.json

# --- signatures and the app's latest.json --------------------------------------------
signed=0
if [ -n "$sign_key" ] || [ -n "${REGULUS_SIGNING_KEY:-}" ]; then
  key_args=()
  [ -n "$sign_key" ] && key_args=(--key "$sign_key")
  # ${arr[@]+"${arr[@]}"}: an empty array is not "unbound" under set -u in bash < 4.4
  "$bin" update sign ${key_args[@]+"${key_args[@]}"} --release-version "$version" manifest.json ${app_files[@]+"${app_files[@]}"} >/dev/null
  signed=1
else
  say "unsigned: no --sign-key nor \$REGULUS_SIGNING_KEY; 'regulus update' will refuse this release"
fi

if [ ${#app_files[@]} -gt 0 ]; then
  if [ "$signed" = 0 ] || [ -z "$url" ]; then
    say "no latest.json: the app's updater needs a signature and --url (or \$REGULUS_UPDATE_URL)"
  else
    {
      printf '{\n  "version": "%s",\n  "notes": "",\n  "pub_date": "%s",\n  "platforms": {' "$version" "$date_utc"
      sep=""
      for name in "${app_files[@]}"; do
        case "$name" in
          *.AppImage) key="$platform" ;;            # Tauri's default for Linux: the AppImage
          *.deb) key="$platform-deb" ;;
          *.rpm) key="$platform-rpm" ;;
          *) key="$platform-${name##*.}" ;;
        esac
        sig="$(base64 -w0 < "$name.sig" 2>/dev/null || base64 < "$name.sig" | tr -d '\n')"
        printf '%s\n    "%s": {"signature": "%s", "url": "%s/%s/%s"}' "$sep" "$key" "$sig" "$url" "$channel" "$name"
        sep=","
      done
      printf '\n  }\n}\n'
    } > latest.json
  fi
fi

# --- report --------------------------------------------------------------------------
total=$(( $(date +%s) - started ))
say "regulus $version · channel $channel · $dest"
for file in *; do
  printf '  %-48s %10s bytes\n' "$file" "$(stat -c %s "$file" 2>/dev/null || stat -f %z "$file")" >&2
done
say "time: cli ${cli_secs}s, app ${app_secs}s, total ${total}s"
if [ -n "$failed" ]; then
  say "not built: $failed"
  exit 3
fi
