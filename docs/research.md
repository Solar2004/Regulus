# Investigación — datos medidos

> **v0.3** · 2026-10-02
> Hechos verificados, no opiniones. **No repitas esta investigación.** Si un dato se
> queda obsoleto, actualízalo aquí con la fecha.

---

## 1. Orca — `stablyai/orca`

Se investigó para responder una pregunta concreta: *¿está hecha en Rust?*
**Respuesta: no.**

| Dato | Valor | Medido |
|---|---|---|
| Licencia / estrellas | MIT · 83.7k ★ | 2026-10-02 |
| Creado | 2026-03-17 (releases diarios) | 2026-10-02 |
| Lenguaje dominante | **TypeScript, 173.1 MB** | 2026-10-02 |
| **Rust** | **6 KB (0.003%)** — un helper nativo suelto | 2026-10-02 |
| Shell de escritorio | **Electron 43** + `electron-builder` + `electron-vite` | `package.json` |
| UI | React 19, Tailwind 4, zustand, Vite (rolldown) | `package.json` |
| Terminal | **xterm.js 6 + `@xterm/addon-webgl`** + `node-pty` | `package.json` |
| Móvil | Swift (iOS), Kotlin (Android) | árbol del repo |

Desglose completo de lenguajes:

```
TypeScript 173.1 MB · JavaScript 7.5 MB · Swift 388 KB · CSS 241 KB · HCL 239 KB
PowerShell 120 KB · HTML 113 KB · Kotlin 109 KB · Shell 66 KB · Python 58 KB
Ruby 27 KB · C++ 25 KB · Dockerfile 12 KB · NSIS 6 KB · Rust 6 KB · C# 4 KB
```

### 1.1 Dos aclaraciones que importan

1. Su README dice *"Ghostty-class terminals with WebGL rendering"*. Es una **comparación
   de marketing**, no una dependencia: no usan Ghostty (que además es Zig, no Rust). El
   motor real es xterm.js con canvas WebGL.
2. Su *"embedded Chromium browser with design mode"* es una `BrowserWindow` de Electron.
   Electron **ya es** Chromium; les salió gratis.

### 1.2 Conclusión

Lo que se percibe como "nativo y rápido" en Orca viene de WebGL en el renderer y de PTYs
en Node, no de Rust.

**Orca sirve como referencia de UX. No como referencia de arquitectura para Regulus.**

---

## 2. hcom — la primera habilidad a consumir

Fork de Alexander: `Solar2004/hcom`. Upstream: `aannoo/hcom` (535 ★).
Copia local: `~/Projectos/hcom` — modificable.

| Dato | Valor |
|---|---|
| Lenguaje | **Rust 5.5 MB** (+ TS 74 KB, Shell 65 KB) |
| Edition / toolchain | 2024 · rust-version 1.86 · versión 0.7.18 |
| Forma del crate | **Un solo `[[bin]]` (`src/main.rs`). NO tiene `[lib]`.** |
| Commits / último | 269 commits · 2026-05-24 |
| Persistencia | `rusqlite` 0.39 (SQLite bundled) |
| TUI | `ratatui` 0.30 + `crossterm` 0.29 |
| PTY / proceso | `nix` 0.31 (term, signal, poll, process, fs, ioctl), `libc`, `vt100` 0.16 |
| Red / relay | `rumqttc` 0.25 (MQTT sobre rustls), `chacha20poly1305`, `sha2`, `base64` |
| CLI | `clap` 4 (derive) |
| Qué hace | Agentes de IA se mandan mensajes, se observan y se generan entre sí vía terminales |

### 2.1 Por qué importa que sea Rust

Es la razón por la que la doctrina Predator
([`01-vision.md` §2](01-vision.md)) es viable: el código se **transplanta**, no se
traduce.

### 2.2 El detalle que condiciona el trabajo

**No tiene `[lib]`.** Es un binario. Para absorberlo como módulo hay que extraer la
lógica de `main.rs` y reescribirla tras el trait `Module`. Alexander ya lo sabe y está
de acuerdo: *"le quitamos todo el código, agarramos la idea y el código, y lo
implementamos en nuestro workflow"*.

Trabajo pendiente: BRIEF-4 en [`05-roadmap.md`](05-roadmap.md). **No tocar hcom
todavía** (decisión #5).

---

## 3. El workflow actual de Alexander

Medido sobre `~/Projectos` el **2026-10-02**. Cada dato se convirtió en un requisito de
diseño en [`01-vision.md` §5](01-vision.md).

| Señal | Dato |
|---|---|
| Proyectos top-level | **55** carpetas · **36** son repos git · **29** tienen commits |
| Nacidos y abandonados | **12 de 29** repos con ≤5 commits (41%) |
| Sin git en absoluto | **19** carpetas |
| Último commit en cualquier proyecto | **2026-09-13** |
| Skills instaladas | **150** `SKILL.md` en `.claude-sub` + **17** en `.claude` · 80 directorios de marketplace |
| Ficheros de contexto | **29** `CLAUDE.md`/`AGENTS.md`, los mayores de 5-10 KB |
| MCP servers | 1 (`alexandria`) |

Repos con ≤5 commits: `AikenCG(1)`, `Ascend(5)`, `AscendToPeak(4)`, `claude-sub(1)`,
`HypnoCentral(1)`, `HypnoLauncher(1)`, `kimi-chat-proxy-rs(2)`, `NextWeb-Studio(1)`,
`numpad-mouse(1)`, `SiteGenerator(2)`, `Solar2004(4)`, `Solaris-Academy(3)`.

Forks paralelos del mismo proyecto: `aicli-ultimate` / `aicli-ultimate-hcom` ·
`thelorian.centaury.net` / `-old` · `Kore-old` / `Centaury/Kore` · `Proteus` /
`ProteusWeb` · `korian/claudian` / `korian/korian`.

### 3.1 Cadena de herramientas

```
Claude Code → headroom :8788 → token-proxy :3456 → muse-spark-1.2-contributor
```

Claude Code pide `claude-opus-4-6[1m]`; el nombre es falso a propósito para que asuma
ventana de 1M y no compacte antes de tiempo. `ccmodel` dice el modelo real. Dos perfiles:
`~/.claude` y `~/.claude-alt` (`ccalt`, de emergencia), más `~/.claude-sub` (el activo en
este proyecto).

Dato relevante para el diseño: **`ccmodel` existe porque el sistema no dice qué modelo
corre.** De ahí el requisito 5 de [`01-vision.md` §5](01-vision.md).
