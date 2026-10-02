# 06 — Stack, terminal y versionado

> **v0.4** · 2026-10-02
> Las dependencias concretas, con versiones verificadas. El reparto de la terminal.
> El sistema de versionado de la app.
> Lee [`02-architecture.md`](02-architecture.md) primero.

---

## 1. Decisión #7 cerrada: Tauri 2

✅ **DECIDIDO** — El argumento decisivo **no es la preferencia, es la terminal.** Es el
componente más difícil de la app y solo hay una implementación probada en batalla.

| Framework | Terminal disponible | Realidad |
|---|---|---|
| **Tauri** (webview) | **xterm.js 6 + `addon-webgl`** | Maduro: WebGL, ligaduras, unicode11, búsqueda, scrollback serializable. Es lo que usa Orca. |
| egui | `egui_term` | Muy joven. Acabarías escribiendo un renderizador VTE. |
| iced | `iced_term` | Igual o peor. |

Escribir un emulador de terminal es un proyecto en sí mismo, y no es el proyecto que
Regulus quiere ser ([`01-vision.md` §3](01-vision.md)).

**Matiz importante:** la terminal **no se construye en v1** (v1 es solo lectura, sin
hcom). Pero decide el framework ahora, porque cambiarlo después es carísimo.

**El precio honesto:** la UI será TypeScript. Core, CLI y módulos siguen en Rust. Por la
decisión #3 (límite externo por socket), si algún día la UI estorba se reescribe en egui
sin tocar el core. Es el componente barato del proyecto, no la caja fuerte.

---

## 2. El stack completo

Versiones verificadas el **2026-10-02**. Actualizar con fecha si cambian.

### 2.1 Core, CLI y módulos (Rust)

| Qué | Crate | Versión | Por qué esta |
|---|---|---|---|
| RPC servidor y cliente | **`jsonrpsee`** | `0.26.1` (upd 2026-09-30) | JSON-RPC 2.0 con WebSocket nativo, servidor **y** cliente. Cubre los dos lados del límite externo. |
| PTY | **`portable-pty`** | `0.9.0` · 18M descargas | De wezterm. Multiplataforma incluido ConPTY en Windows. |
| Agentes | **`agent-client-protocol`** | `2.2.0` (upd 2026-09-18) | Ver [`07-ecosystem.md`](07-ecosystem.md). Da 45 agentes sin escribir integraciones. |
| MCP | **`rmcp`** | `3.5.0` (upd 2026-09-28) · 31M descargas | SDK oficial de MCP para Rust. Sirve UC-4 y UC-5. |
| Store | **`rusqlite`** | `0.39` (bundled) | Lo mismo que ya usa hcom, así el código absorbido encaja. |
| Async | **`tokio`** | `1.x` | Lo exigen `jsonrpsee` y `rmcp`. |
| Serialización | **`serde`** + **`serde_json`** | `1.x` | Obvio. |
| Errores | **`thiserror`** (libs) + **`anyhow`** (bins) | `2` / `1` | Convención estándar; hcom ya la sigue. |
| CLI | **`clap`** `derive` | `4` | Lo mismo que hcom. |

Aviso honesto: **`portable-pty` no se actualiza desde 2025-02-11.** No es abandono —
manejar PTYs no cambia, y es lo que mueve wezterm en producción. Si alguna vez estorba,
la alternativa es `nix` directo, que es lo que hcom hace hoy.

### 2.2 Cliente de escritorio

| Qué | Elección | Versión | Por qué esta |
|---|---|---|---|
| Shell de escritorio | **`tauri`** | `2.12.1` (upd 2026-10-01, ayer) | §1 |
| Frontend | **React 19** | — | Mejor integración documentada con xterm.js; los agentes a los que se delega diseño escriben React mejor que alternativas. |
| Build | **Vite** | — | Estándar con Tauri. |
| Estilos | **Tailwind 4** | — | Rápido de iterar; el diseño de BRIEF-2 se traduce directo. |
| Estado | **zustand** | — | Ligero. El estado real vive en el core; el cliente solo cachea. |
| Terminal | **`@xterm/xterm` + `addon-webgl`** | `6.0.0` **estable** | Orca va con `6.1.0-beta.304`; aquí se puede ir con la estable. |
| Grafo (topología) | **`dagre`** + SVG a pelo | — | Ver §2.3. |

### 2.3 Por qué NO React Flow para la vista topología

React Flow (`@xyflow/react`) es para grafos **editables**: arrastrar nodos, cablear a
mano. La vista topología de Regulus se **deriva de los manifests** y es de solo lectura
([`04-client.md` §2.2](04-client.md)).

`dagre` calcula el layout jerárquico y el resto son ~100 líneas de SVG. Se ahorra una
dependencia pesada y, más importante, se evita que la UI sugiera que puedes cablear con
el ratón cuando no puedes.

---

## 3. La terminal: cómo se reparte

✅ **DECIDIDO**

```
  webview (TypeScript)      │  core (Rust)
  ──────────────────────────┼─────────────────────────────────
  xterm.js 6.0.0            │  portable-pty 0.9.0
  + @xterm/addon-webgl      │  lee y escribe el PTY real
  + addon-fit               │  supervisa el proceso hijo
  + addon-unicode11         │
  + addon-search            │
  + addon-serialize         │
         ▲                  │        │
         └──── bytes ───────┴────────┘
                  WebSocket, binario
```

**El webview solo pinta. El PTY vive en Rust, del lado del core.**

Tres consecuencias que importan, y es por esto que el reparto va así y no al revés:

1. **Un agente sigue corriendo con la ventana cerrada.** En Orca el PTY está en Node, en
   el proceso de la app; cerrar la ventana mata al agente. Aquí no.
2. **El móvil puede adjuntarse al mismo PTY** que la app de escritorio, porque el PTY no
   pertenece a ningún cliente.
3. **El scrollback sobrevive a reinicios del cliente**, porque `addon-serialize` guarda
   contra el core, no contra la ventana.

Esto es una mejora real sobre el diseño de Orca, no una copia.

---

## 4. Decisión #8 cerrada: transporte

✅ **DECIDIDO** — **JSON-RPC 2.0 sobre WebSocket en `127.0.0.1`.**

| Aspecto | Elección |
|---|---|
| Transporte | WebSocket (`jsonrpsee` ws server) |
| Bind | `127.0.0.1` únicamente. Nunca `0.0.0.0` sin decisión explícita. |
| Auth | Token en `~/.regulus/token`, modo `600`, generado al primer arranque |
| Puerto | Fijo por defecto, configurable; escrito en `~/.regulus/port` para que los clientes lo descubran |
| Binario | Los bytes de terminal van como frames binarios del mismo WebSocket |

**Por qué WebSocket y no socket Unix**, que sería lo más idiomático en Linux: el webview
de Tauri no puede abrir un socket Unix directamente. Y WebSocket hace que el acceso
remoto sea **tunelizar**, no reimplementar.

Un solo transporte para los tres clientes (GUI, CLI, móvil). Si algún día hace falta un
socket Unix para el CLI, se añade como segundo listener sin tocar el protocolo.

---

## 5. Sistema de versionado

✅ **DECIDIDO** — Hay **tres ejes** que se versionan por separado, y solo uno tiene
dientes de verdad.

| Eje | Dónde vive | Esquema | Rigor |
|---|---|---|---|
| **App** | `version` del workspace en `Cargo.toml` | SemVer `0.x` hasta que exista v1 | Flojo. Es un número para tags y releases. |
| **Protocolo** | `regulus-proto` | **SemVer estricta** | **El único crítico.** |
| **Módulo** | `regulus-module.toml` | SemVer + `requires_proto` | Medio. El registro lo valida al cargar. |

### 5.1 Por qué el protocolo es el que importa

Cliente y core **se despliegan por separado** a través del límite externo. El caso que lo
hace real es el móvil: una app en la App Store se actualiza cuando Apple quiere, no
cuando tú quieres. Sin versión de protocolo negociada, un día abres el móvil, no conecta,
y no hay forma de saber por qué.

Handshake obligatorio al conectar, antes de cualquier otro método:

```json
→ {"method":"hello","params":{"client":"regulus-gui/0.2.0","proto":"1.3"}}
← {"result":{"core":"regulus/0.4.1","proto":"1.4","compatible":true}}
```

Tres reglas:

1. **Mismo major = compatible.** El core soporta el proto `N` y el `N-1`.
2. **Major distinto → el cliente no arranca** y dice exactamente qué versión necesita.
   No se degrada en silencio.
3. **Método nuevo = minor. Quitar o cambiar un método = major.** Sin excepciones.

### 5.2 Versionado de módulos

```toml
[module]
id      = "agents"
version = "0.1.0"
requires_proto = "^1"      # el registro lo valida antes de cargar
```

Si no encaja, el módulo no se carga y entra en un estado nuevo de la máquina de
[`03-modules.md` §3](03-modules.md):

```rust
Incompatible { requires: String, have: String }
```

La UI ya sabe pintarlo: es el mismo tratamiento que `Disabled` y `Broken`
([`04-client.md` §4](04-client.md)). El versionado se enchufa en la máquina de estados
que ya existía, no añade un sistema paralelo.

### 5.3 Releases

| Qué | Herramienta | Nota |
|---|---|---|
| CLI (`regulus`) | **`cargo-dist`** `0.32.0` | Ya se usa en hcom (`dist-workspace.toml`), así que el camino es conocido |
| App de escritorio | **`tauri build`** | Tauri trae su propio empaquetador |
| Disparador | un `git tag v0.x.y` | Un tag produce los dos artefactos |

❓ **ABIERTO** — Si habrá canal nightly. No se decide hasta que haya algo que publicar.
