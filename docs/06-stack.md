# 06 — Stack, terminal y versionado

> **v0.6** · 2026-11-08 (§5.4 build y actualización; publicación en GitHub Releases y valores por defecto de `[update]`)
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
Regulus quiere ser ([`plan.md` §4.2](plan.md)).

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
| Agentes | **`agent-client-protocol`** (feature `process`) | `3.4.0` (upd 2026-10-16) | Canal de los motores ([`12-agents.md`](12-agents.md) §4). 43 agentes sin escribir integraciones. |
| Async sin runtime | **`futures`** | `0.3` | Cada sesión ACP corre en un hilo propio con `futures::executor`: los módulos son síncronos y no necesitan tokio. |
| Adaptador ACP de Claude | **`@agentclientprotocol/claude-agent-acp`** (npm) | lo fija el campo `adapter` de `crates/modules/engine-claude/regulus-module.toml` | Se ejecuta con `npx`; necesita Node. Ver [`12-agents.md`](12-agents.md) §4. |
| MCP | **JSON-RPC por stdio, implementación propia** | — | Servidor `tools-mcp` y cliente del motor nativo; herramientas, no todo MCP (`12-agents.md` §8). `rmcp` estaba elegido en el diseño, pero no se incorporó a Cargo. |
| Store | **`rusqlite`** | `0.39` (bundled) | Lo mismo que ya usa hcom, así el código absorbido encaja. |
| Async | **`tokio`** | `1.x` | Lo exige `jsonrpsee` en el servidor. |
| Serialización | **`serde`** + **`serde_json`** | `1.x` | Obvio. |
| Errores | **`thiserror`** (libs) + **`anyhow`** (bins) | `2` / `1` | Convención estándar; hcom ya la sigue. |
| CLI | **`clap`** `derive` | `4` | Lo mismo que hcom. |
| Middleware HTTP | **`tower`** | `0.5` | La puerta del token en el upgrade; es la versión que usa `jsonrpsee` 0.26. |
| Token aleatorio | **`getrandom`** | `0.4` | Bytes del sistema operativo para `<home>/token`, sin arrastrar `rand`. |
| Config y manifiestos | **`toml`** | `1` | `config.toml` y `regulus-module.toml`. |
| TUI | **`ratatui`** (trae **`crossterm`**) | `0.30.2` · crossterm `0.29.0` (verif. 2026-10-18) | El estándar en Rust para interfaces de terminal. `crossterm` se usa a través de `ratatui::crossterm`, así las dos versiones no se desincronizan. [`04-client.md`](04-client.md) §7. |
| Expresiones regulares | **`regex`** | `1.13` (1.13.1, verif. 2026-11-07) | Reglas `regex` de `switch` y `loop` en `flow`. Ya estaba en el lock por otras dependencias. |
| Globs | **`glob`** | `0.3.4` (verif. 2026-11-07) | `read` de `files` con `specs/*.md`. Del equipo de Rust, sin dependencias. |
| Observar ficheros | **`notify`** | `8.2` (8.2.1, verif. 2026-11-07; licencia CC0-1.0) | `watch` de `files`: inotify, FSEvents y ReadDirectoryChanges detrás de una API. La 9 sigue en `rc`. |
| Señales a grupos de procesos | **`libc`** | `0.2` (`0.2.191`, verif. 2026-11-07) | `killpg` para que `process` mate el grupo entero de un comando o job, y para que el core pare el de un módulo local (`regulus-core`, `03-modules.md` §6); solo Unix. Ya venía por `portable-pty`. |
| Cliente HTTP de los módulos | **`ureq`** (features `json`; TLS `rustls` sobre `ring`) | `3.5.1` (upd 2026-11-02, verif. 2026-11-07) | Síncrono, sin runtime de tokio por cliente: encaja con módulos de hilos. Lo usa el motor LLM de `judge`. `reqwest` solo estaba en el árbol por Tauri, sin `blocking` y con TLS `aws-lc-rs`. |
| Parser HTTP de los módulos | **`httparse`** | `1.10.1` (2025-03-03, verif. 2026-11-07) · 870M descargas | Cabecera de las peticiones del Webhook de `triggers`, que sirve sobre `std::net` con un hilo y un plazo por conexión: un cliente que se para no bloquea a nadie y cerrar no espera a ningún cliente. Ya estaba en el árbol (`hyper`). `tiny_http` se descartó: no tiene plazo de lectura. |
| Cron | **`croner`** | `4.0.3` (upd 2026-11-04, verif. 2026-11-07) | Cinco o seis campos, `L`, `#`, `W`, alias (`@hourly`), zonas. El Cron de `triggers`. |
| Fechas | **`chrono`** (sin default; `clock`, `std`) | `0.4.45` (verif. 2026-11-07) | Lo pide `croner`; ya estaba en el árbol. |
| Texto de PDF | **`lopdf`** (sin default) | `0.47.0` (la última, verif. 2026-11-07) | `pdf` de `web`: Rust puro, `extract_text_with_limit` acota lo que descomprime cada página. No había lector de PDF en el árbol; `pdf-extract` 0.12 se apoya en `lopdf` y suma más crates. Trae ~30 crates nuevos (cifrado de PDF, `nom`, `brotli`). |
| URLs | **`url`** | `2.5.8` (verif. 2026-11-07) | `web`: resolver redirecciones relativas, armar la query del motor de búsqueda y quitar query y credenciales de los errores. Ya estaba en el árbol. |
| Hash de las llaves | **`sha2`** | `0.10.9` (verif. 2026-11-07) | SHA-256 de las llaves `rgk_` de `service`: el store guarda solo el hash. Ya estaba en el árbol (`0.10.9` y `0.11.1`). |
| Requisitos de versión | **`semver`** | `1.0.28` (verif. 2026-11-08) | `requires` de las plantillas (`>=0.1`, `^0.2`) contra la versión del manifiesto de cada módulo. El de cargo, de dtolnay. Ya estaba en el árbol (Tauri). |
| Firma de releases | **`ed25519-dalek`** | `3.0.0` (verif. 2026-11-08) | `regulus update keygen` y `sign`: firma Ed25519. Solo firma; verificar lo hace `minisign-verify`. |
| Hash previo de la firma | **`blake2`** | `0.11.0` (verif. 2026-11-08) | BLAKE2b-512 del fichero antes de firmarlo: el modo *prehashed* (`ED`) de minisign. La 0.11 comparte `digest` 0.11 con `ed25519-dalek`. |
| Verificar firmas | **`minisign-verify`** | `0.3.0` (verif. 2026-11-08) · sin dependencias | `regulus update` comprueba la firma del manifiesto. Es el mismo crate con el que verifica `tauri-plugin-updater`: una clave, un formato. |
| Reemplazar el ejecutable | **`self-replace`** | `1.5.1` (verif. 2026-11-08) | Pone el binario nuevo en lugar del que corre. En Unix es copiar junto a él y `rename` (atómico); en Windows resuelve que un `.exe` en marcha no se puede pisar. Se prefirió a `self_update` 1.3.3, que trae su propio cliente HTTP, `zip`, `tar` y formatos de GitHub/S3: lo que Regulus ya tiene (`ureq`) o no quiere. |
| Base64 | **`base64`** | `0.22` (verif. 2026-11-08) | Claves y firmas en texto (formato minisign). Ya estaba en el árbol. |

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
| Canvas de composición | **`@xyflow/react`** | `12.12.0` (verificado 2026-10-04) | Ver §2.3. |
| Mock de desarrollo | **`ws`**, tipos `@types/ws` | `8.22.0` / `8.18.2` (verificado 2026-10-04) | JSON-RPC por WebSocket integrado en Vite; no entra en el cliente de producción. |
| Tests unitarios del cliente | **`vitest`** | `5.1.0` (verificado 2026-11-08) | Usa la config de Vite (alias `@`), sin otro bundler; `pnpm test` corre la lógica pura de `src/`. |
| Actualizar la app | **`tauri-plugin-updater`** (Rust) | `2.14.1` (verif. 2026-11-08) | El updater de Tauri, configurado en tiempo de ejecución desde `[update]` de `config.toml` (§5.4): sin URL ni clave fijas en `tauri.conf.json`. Solo en el shell: el frontend lo pide con comandos de Tauri, sin el paquete JS. |

### 2.3 Canvas de composición con React Flow

**Actualización 2026-10-04:** Alexander pidió empezar el canvas de nodos y workflows de
[`11-composition.md`](11-composition.md), siguiendo las fases A' y B' de
[`plan.md` §3](plan.md). Se adopta React Flow para este incremento visual.
Esto no aprueba por sí solo todos los contratos abiertos de composición.

El descarte anterior dependía de una topología derivada de manifests y siempre de solo
lectura. El nuevo requisito incluye editar nodos y cables en D'; React Flow permite
conservar el renderizador al llegar a esa fase. A'/B' permiten seleccionar, inspeccionar,
mover la cámara y hacer zoom; crear, mover o conectar nodos sigue desactivado.

Los nodos se renderizan de forma genérica desde los datos del servidor. Las posiciones
viven en el perfil de ejemplo, los colores en los tokens de diseño. El mock sirve estado
del sistema por JSON-RPC, sin importar React Flow. Contrato y límites del incremento:
[`composition-preview.md`](composition-preview.md).

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
([`04-client.md` §3](04-client.md)). El versionado se enchufa en la máquina de estados
que ya existía, no añade un sistema paralelo.

### 5.3 Releases

| Qué | Herramienta | Nota |
|---|---|---|
| CLI (`regulus`) | **`cargo-dist`** `0.32.0` | Ya se usa en hcom (`dist-workspace.toml`), así que el camino es conocido |
| App de escritorio | **`tauri build`** | Tauri trae su propio empaquetador |
| Disparador | un `git tag v0.x.y` | Un tag produce los dos artefactos |

Hoy los dos artefactos salen de `scripts/release.sh` (§5.4); `cargo-dist` sigue siendo el
camino para publicar en varias plataformas desde CI cuando haga falta.

❓ **ABIERTO** — Si habrá canal nightly. No se decide hasta que haya algo que publicar.

### 5.4 Build y actualización (2026-11-08)

✅ **HECHO** — un script construye la release y los dos clientes se actualizan solos desde
una URL que se configura.

**Build: `scripts/release.sh`.** Construye el programa `regulus` (core, CLI y TUI) con
`cargo build --release -p regulus-cli` y la app con `tauri build` (por defecto AppImage y
deb), y escribe `dist/<versión>/`:

| Fichero | Qué es |
|---|---|
| `regulus-<so>-<arch>` | El programa (`regulus-linux-x86_64`). El sufijo deja publicar varias plataformas en el mismo canal |
| `Regulus_<v>_amd64.AppImage`, `.deb` | Los bundles de la app que se hayan construido |
| `SHA256SUMS` | `sha256sum` de todo lo anterior |
| `manifest.json` | `{version, date, channel, files: [{name, sha256, size, platform, kind}]}`: lo que lee `regulus update`. `kind` es `cli` o `app`; un `url` opcional por fichero (absoluto o relativo al manifiesto) lo saca de la carpeta del canal |
| `*.sig` | Firma minisign del manifiesto y de cada bundle (con `--sign-key` o `$REGULUS_SIGNING_KEY`) |
| `latest.json` | El formato del updater de Tauri, con firma y URL de cada bundle; solo con firma y `--url` (o `$REGULUS_UPDATE_URL`), porque Tauri exige URLs absolutas |

- **Una sola versión:** la de `[workspace.package]` en `Cargo.toml`. `regulus-tauri` la hereda
  (`version.workspace = true`) y `tauri.conf.json` ya no lleva `version`: Tauri toma la del
  crate. El script comprueba que `regulus --version` dice la misma y para si no.
- **Flags:** `--cli-only`, `--channel <nombre>` (`stable` por defecto; cualquier nombre de
  carpeta: el canal nightly sigue ❓ abierto, pero no hay que tocar código para tenerlo),
  `--out <dir>`, `--sign-key <fichero>`, `--url <base>`, `--bundles <lista>`.
- **Sin librerías del sistema** (`pkg-config`: webkit2gtk-4.1, javascriptcoregtk-4.1,
  libsoup-3.0, gtk+-3.0, librsvg-2.0, openssl; lista en `$REGULUS_APP_LIBS`) o sin `pnpm`, la
  app se salta diciendo qué falta y el programa sale igual. Sale con código 3 si la app no se
  construyó. No instala nada del sistema.
- **Publicar** es copiar `dist/<versión>/*` a `<url>/<canal>/` en cualquier servidor estático.
- **Dónde se publica Regulus** (2026-11-08): en **GitHub Releases**, sin servidor propio.
  `.github/workflows/release.yml` corre con cada tag `v<versión>` (o a mano, eligiendo canal):
  comprueba que el tag es la versión de `Cargo.toml`, instala las librerías de la app en
  `ubuntu-22.04` (la glibc más vieja que ofrece GitHub, para que el binario arranque en más
  máquinas), ejecuta `release.sh` firmando con el secreto `REGULUS_SIGNING_KEY` (el texto de
  la clave de `keygen`) y sube los ficheros **dos veces**: a la release `v<versión>`, que se
  queda para siempre, y a la release **`<canal>`** (`stable`; `beta` si la versión lleva
  `-`), que se actualiza en el sitio con cada publicación (primero suben los ficheros nuevos,
  luego se quitan los de la versión anterior y el tag sigue al commit publicado: su URL nunca
  da 404 entre dos versiones). Esa segunda es la carpeta del
  canal: GitHub sirve sus ficheros en `releases/download/<canal>/<fichero>`, justo la forma
  `<url>/<canal>/…` que leen `regulus update` y el updater de Tauri. Un canal nuevo es solo
  otro nombre de release. Sin el secreto, el workflow para antes de construir.
- **Medido** (2026-11-08, Arch, con otros dos builds en la máquina y `nice 10`): `regulus` en
  release 7 min 07 s (`lto = true`, `codegen-units = 1`); la app 4 min 41 s de compilación
  más ~1,5 min de bundles. Tamaños: `regulus-linux-x86_64` 21,2 MiB, `.deb` 5,8 MiB,
  `.AppImage` 102,8 MiB. El AppImage baja `linuxdeploy` de GitHub la primera vez: sin red
  falla solo ese bundle (cada tipo se empaqueta aparte con `tauri bundle`).

**Claves.** `regulus update keygen --out release.key` escribe la clave secreta (modo 600) y
`release.key.pub`, e imprime la línea para `config.toml`. El formato es el de **minisign**
(Ed25519 con BLAKE2b-512 previo): una sola clave firma el manifiesto que lee la CLI y los
bundles que lee el updater de Tauri, y `minisign -V` puede comprobar cualquier firma.
`regulus update sign <ficheros> --key <f>` (o `$REGULUS_SIGNING_KEY` con el texto de la clave)
escribe `<fichero>.sig` con `version:<v>` en el comentario firmado, que Tauri compara con la
versión que anuncia `latest.json`. La clave secreta no va al repo (`*.key` en `.gitignore`).

**`[update]` en `config.toml`** (es el único sitio de estas claves):

| Clave | Por defecto | Qué hace |
|---|---|---|
| `url` | `https://github.com/Solar2004/Regulus/releases/download` | Base de las releases: el manifiesto es `<url>/<channel>/manifest.json`. Por defecto las del proyecto en GitHub Releases (arriba). **`url = ""` apaga la actualización**, y `regulus update` lo dice con la clave a poner. Vale `https://`, `file://` o una ruta; `http://` solo con `allow_insecure` |
| `channel` | `stable` | Qué carpeta seguir (`stable`, `beta`, …) |
| `check_on_start` | `false` | Al arrancar `regulus serve` (en un hilo, sin retrasar el arranque) y al abrir la app, mira si hay una versión nueva y lo dice. Nunca instala |
| `public_key` | la clave de las releases de Regulus (`RWSd0JkM…`, `config.rs`) | La línea `RW…` que imprimió `keygen`. Por defecto la que firma las releases del proyecto; quien publique su propio canal pone la suya. **Vacía no se instala nada**; `--check` funciona y avisa "unverified" (la firma no se comprobó) |
| `timeout_ms` | 120000 | Cuánto puede tardar una descarga. `0` = sin límite |
| `max_bytes` | 1073741824 (1 GiB) | Lo máximo que puede pesar un fichero de la release; un manifiesto que pida más se rechaza sin descargar |
| `allow_insecure` | `false` | Aceptar `url` en `http://` plano. Apagado, CLI y app rechazan esa URL antes de pedir nada. La firma ya protege el contenido; esto evita seguir una URL mal escrita |

**CLI: `regulus update [--check] [--channel X] [--url X] [--yes]`.** Baja el manifiesto y su
`.sig`, verifica la firma, exige que el manifiesto firmado nombre su canal y sea el que se
sigue (uno firmado para `beta` no vale copiado en `stable`), compara SemVer con la versión
que corre (solo sube, nunca baja), elige el fichero `kind = cli` de su plataforma
(`std::env::consts`, `linux-x86_64`), lo baja y comprueba tamaño y sha256 contra el manifiesto
firmado. Lo escribe junto al ejecutable en un fichero nuevo con nombre aleatorio y modo
`0700` (nunca encima de algo que ya esté ahí), lo ejecuta con `--version` sin stdin y con
30 s de tope (tiene que decir la versión nueva: así no se instala un binario que no arranca
aquí) y lo renombra encima con `self-replace`, con los permisos del programa viejo.
Cualquier fallo deja el programa como estaba. **Límite:** no hay protección contra *replay*:
un manifiesto firmado antiguo sigue siendo válido. No permite bajar de versión porque solo
se instala `version >` la que corre, pero sí serviría para retener a alguien en una versión
vieja mientras se le sirva ese manifiesto. Sin `--yes` pregunta, y sin
terminal pide `--yes`. Si la carpeta del ejecutable no se puede escribir (instalado por el
gestor de paquetes), lo dice: ese caso se actualiza con el paquete.

**App.** El shell lee `[update]` del mismo `config.toml` y construye el updater de Tauri en
tiempo de ejecución: endpoint `<url>/<channel>/latest.json` y, como clave, la misma
`public_key` envuelta en el fichero de clave pública de minisign que Tauri espera. Ajustes →
General → *Updates* muestra la versión que corre, la disponible y el botón para instalarla
(`04-client.md` §10); instalar vuelve a leer `latest.json` y, si ahora anuncia otra versión
que la mostrada, no instala y pide comprobar de nuevo. Sin `url`, dice que está apagado y qué poner.
`tauri.conf.json` solo lleva lo que no se puede dar en tiempo de ejecución:
`requireSignedVersion = true` (la versión firmada tiene que ser la anunciada) y
`dangerousInsecureTransportProtocol = true`, para que `url` pueda ser `http://` como en la
CLI: la integridad la da la firma, no TLS, y una release no tiene nada secreto.

**Probado:** `crates/regulus-cli/tests/update.rs` sirve una release con `python3 -m
http.server` y comprueba que una copia de `regulus` se reemplaza por el binario nuevo y que
un manifiesto alterado tras firmarlo, otra clave, un sha256 equivocado o la falta de clave lo
dejan intacto.
