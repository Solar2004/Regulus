# 04 — El cliente (app y CLI)

> **v1.0** · 2026-10-05 · Lo vigente del cliente, rescatado de la versión anterior
> ([`old/04-client.md`](old/04-client.md), modelo antiguo: 8 métodos, agente en el centro).
> El contrato de métodos actual está en [`composition-preview.md`](composition-preview.md).

---

## 1. El mock es la spec del core

✅ **DECIDIDO** (decisión #9, cliente primero) — La UI se construye contra un mock, y la
lista de métodos del mock **es** la especificación del core.

- **El mock no inventa métodos cómodos para la UI.** Cada método devuelve algo que el
  core puede servir de verdad, con datos que un módulo realmente tiene.
- **Los métodos devuelven estado del sistema**, no objetos preformados para un componente.
  La UI compone.
- **Prueba:** al conectar el core real, la UI no cambia una línea. Si cambia, algún método
  del mock era falso.

---

## 2. Las dos vistas

✅ **DECIDIDO** — Dos proyecciones sobre **un solo modelo de estado**, no dos apps:

| Vista | Para qué |
|---|---|
| **Canvas** | Diseñar: nodos, cables y workflows ([`11-composition.md`](11-composition.md)) |
| **Operar** | Lo que corre ahora: ejecuciones, terminales, logs, errores |

El canvas puede marcar qué nodos están vivos, pero no mezcla estado con diseño.

---

## 3. Estados de primer nivel

✅ **DECIDIDO** — Cada uno se diseña, no es un error genérico:

1. **No instalado** — un módulo ausente se muestra así, **nunca como error**.
2. **Desactivado** — le falta una entrada `required`; se dice cuál.
3. **Roto** — pánico o fallo en `init`; se muestra la razón y se puede recargar.
4. **Sin usar** — `Idle` con `last_used` vacío: instalado y nunca usado, visible.

Iconos y colores de cada estado: [`08-design-system.md`](08-design-system.md).

---

## 4. Conexión con el core

✅ **DECIDIDO** (decisión #8)

- **Transporte:** JSON-RPC 2.0 sobre WebSocket en `127.0.0.1`, token en `~/.regulus/token`.
- **`hello` primero, siempre.** Mismo major de protocolo = compatible; el core soporta N y
  N-1. Con major distinto el cliente no arranca ni se degrada en silencio: dice qué
  versión necesita. Vale también para el mock.
- **Arranque:** la app nunca lanza un segundo core si el puerto responde a `hello`. El core
  auto-lanzado se desprende (cerrar la ventana no lo mata). `autostart = false` lo
  desactiva; sin core, la app muestra el comando exacto a ejecutar.
- **Hecho (2026-10-18):** la conexión es una sola para toda la app (`src/core/connection.ts`).
  En el escritorio, el shell lee `port` y `token` del home, la UI dice `hello` y, si nadie
  responde, el shell lanza `regulus --home <home> serve` en su propio grupo de procesos y
  espera el puerto nuevo. Lo configura `[client]` de `config.toml` (claves y quién las lee:
  §11). Lo que imprime el core lanzado va a `<home>/core.log`. En un
  navegador sigue igual: `VITE_RPC_URL` o el mock. La lógica vive en `regulus_core::local`
  para que la TUI la use también.

---

## 5. Multi-proyecto

✅ **DECIDIDO** (decisión #15) — El proyecto activo es **estado del cliente**. Los
métodos de lectura aceptan `project_id` opcional (ausente = todos los proyectos). Así dos
clientes miran proyectos distintos contra el mismo core.

---

## 6. Idioma y tema

✅ **DECIDIDO** (decisión #20)

- **UI en inglés**, con todos los textos visibles en ficheros de traducción desde el día 1
  (`frontend/src/locales/`). Prohibido escribir textos visibles directamente en un `.tsx`.
- **Solo tema oscuro visible**, pero todos los colores pasan por variables CSS de
  [`08-design-system.md`](08-design-system.md). Prohibido hardcodear colores.

---

## 7. La TUI

✅ **DECIDIDO** (2026-10-18, Alexander) — `regulus` sin comando, en una terminal, abre una
interfaz a pantalla completa hecha con `ratatui` (versión en
[`06-stack.md`](06-stack.md) §2.1). Es un cliente más: vive en el mismo binario, encuentra
o lanza el core con `regulus_core::local` (§4) y habla el mismo protocolo que la app. Si
la entrada o la salida no son una terminal (scripts, pipes), imprime la ayuda y sale con
código 2, como antes; los comandos (`run`, `msg send`…) no cambian.

- **Primera versión: la vista Operar.** Perfiles (Enter ejecuta), ejecuciones (`s` para),
  nodos de la ejecución con su estado (§3) y, a la derecha, lo que dice el nodo elegido:
  la conversación con un agente (`agent.prompt`) o el canal con sus participantes
  (`comm.history`, `comm.participants`, `comm.send` como `user`). Cada acción tiene su
  comando de CLI.
- **Qué es cada nodo se sabe por capacidad, nunca por módulo:** agente = tiene una entrada
  `agent.engine`; canal = tiene una entrada `participant`. Sin el módulo, el nodo sale
  como *no instalado* y lo demás sigue.
- **La intro:** REGULUS letra a letra, brilla y se desvanece. No retrasa nada: la conexión
  ocurre mientras tanto, cualquier tecla la salta, y con el core ya en marcha dura
  < 1 s (lo comprueba un test). `client.intro = false` la apaga.
- **Refresco:** sin bus todavía, pregunta al core cada `client.refresh_ms` (1000) qué
  corre y, si hay un canal elegido, sus mensajes.
- **Colores:** los de `assets/design/palette.json`, el mismo fichero del que salen los
  tokens de la app. Un estado es siempre símbolo + palabra, nunca solo color; en la
  terminal no hay SVG, así que el símbolo es un glifo.
- **Textos en inglés en el código**, como los de la CLI. Si llega la traducción a la
  terminal, se sacan a un fichero.
- La conversación con un agente es de la sesión de la TUI: el core aún no guarda los
  turnos. Las ejecuciones necesitan `profile_id` (protocolo 1.4) para saber sus nodos.

**Segunda versión (2026-11-08).** La TUI hace lo que la app (§8, §9), con los mismos
métodos; ninguno nuevo en el core.

- **Editar** (`e` sobre un perfil, `n` para uno nuevo: nombre → id). Tres paneles: el árbol
  workflow → nodos; el inspector (nombre, cada campo del manifiesto según su tipo —texto,
  número, `select`/bool, `workflow`, `map` como JSON, lo desconocido como texto crudo— con
  su default y *reset*, el workflow interno; con un workflow elegido, sus puertos `self`);
  y los cables del workflow. La paleta sale de `module.manifest` con el estado de cada
  módulo (§3): lo no instalado se ve y no se añade; los workflows con puertos entran como
  caja. Un cable se elige salida → entrada y cada entrada dice si encaja o por qué no (las
  razones de `connect.ts`). Los nodos nuevos se colocan en cascada (paso 40, huella de la
  app) para que el canvas no los solape. Guardar es `profile.save` (sin `force` si es
  nuevo) con sus `diagnostics`; comprobar, `profile.check` sobre lo guardado; ejecutar,
  `execution.start` del workflow elegido; salir con cambios pregunta. El documento se
  edita con funciones puras (`tui/document.rs`, `tui/catalog.rs`) con tests contra
  `check_profile` del core.
- **Plantillas** (`t`). `template.list` con `requires` marcado (instalado / no instalado /
  roto) y variables con su default; Enter abre el formulario (`*` obligatoria, `id`
  opcional, *replace* = `force`) y aplica con `template.apply`. El error se dice por su
  código (`-32004`…`-32007`, `-32602`). Tras aplicar: `e` lo abre en Editar, `Ctrl+R` lo
  ejecuta y vuelve a Operar.
- **Operar gana** abajo los **eventos** de la ejecución elegida (`event.list` con cursor
  `since`/`next`, valor compacto, `/` filtra por nodo, ↑↓ desplaza) y las **decisiones**
  de `human` (`human.list`; `y` aprueba, `n` rechaza con `human.answer`). `w` manda un
  valor a una entrada propia del workflow (`workflow.send`: `text@` lleva el texto, lo
  demás JSON). Sin el módulo: *□ not installed* (`-32601`) o *✗ unavailable* con la razón
  (`-32002`), por el código, nunca por el id del módulo.
- **Teclas y números configurables** en `tui/keys.rs` (`Keys`, `Tuning`), leídos de una
  tabla opcional con `keys` y `tui` (`tui::run_with`). Por defecto: `e` editar, `n` nuevo,
  `t` plantillas, `/` filtro, `y`/`n` decidir, `w` enviar; en Editar `a` añadir, `d` borrar,
  `w` workflow, `r` renombrar, `c` cable, `x` quitar, `p` puerto, `u` reset, `Ctrl+S`
  guardar, `k` comprobar, `Ctrl+R` ejecutar, Enter editar la fila del inspector, `Esc`
  volver (y `Ctrl+C` con cambios pregunta). Dos acciones del mismo contexto en la misma
  tecla es un error al arrancar; `n` rechaza solo si hay algo que rechazar.
  Números: `event_page` 200, `event_keep` 1000, `value_max` 240, `cascade_step` 40,
  `node_width` 250, `node_height` 160. Se cambian en `[client.keys]` y `[client.tui]` de
  `config.toml` (§11).

---

## 8. Operar e instalar plantillas en la app (2026-11-08)

✅ **HECHO** — Dos vistas nuevas en la barra lateral, **Operar** y **Plantillas**, sobre la
misma conexión (`src/operate/`, `src/templates/`). Hacen lo que la TUI (§7) y la CLI, con
los mismos métodos; ninguno es nuevo.

- **Operar.** Perfiles (`profile.list` + `profile.get`) con *Run* (`execution.start`, con
  selector si el perfil tiene varios workflows) y filtro de proyecto como estado del
  cliente (§5). Ejecuciones (`execution.list`) con estado, perfil, workflow, `parent` y
  *Stop* (`execution.stop`). De la elegida: sus nodos con el estado de primer nivel (§3),
  cruzando el workflow con `module.list` y la lista `disabled` (dice qué entrada falta);
  los eventos del bus (`event.list` con `execution_id`, cursor `since`/`next`), con
  filtro por nodo y el valor compacto (texto como texto, JSON en una línea; "mostrar más"
  o JSON con sangría al abrirlo); un formulario para las entradas propias del workflow
  (`workflow.send`; la respuesta sale en los eventos como `self.<salida>`). Un nodo con
  entrada `agent.engine` abre una conversación (`agent.prompt`, turnos de esta sesión);
  uno con entrada `participant`, el canal (`comm.history`, `comm.participants`,
  `comm.send` como `user`). Las decisiones pendientes del módulo `human` (`human.list`,
  `human.answer`) se aprueban o rechazan ahí. Sin el módulo, el panel dice *no instalado*
  (`-32601`) o *no disponible* con la razón del core (`-32002`), nunca un error genérico.
- **Plantillas.** `template.list` con descripción, módulos de `requires` marcados como
  instalado / no instalado / roto (de `module.list`; el rango de versión lo juzga el
  core) y variables con su valor por defecto; una plantilla con `error` lo enseña tal
  cual. *Show file* es `template.get`. *Apply* arma el formulario de `vars` (obligatorias
  marcadas, defaults rellenos), `id` opcional y *reemplazar* (`force`), y llama a
  `template.apply`: enseña los `diagnostics` o el error del core según su código
  (`-32004`…`-32007`, `-32602`). Tras aplicar: *Open in canvas* (la vista de composición
  con ese perfil, `openInCanvas` del store) y *Run now* (`execution.start`). Inicio
  enlaza aquí.
- **Ajustes de la vista** (botón *View settings*, guardados en el dispositivo como la
  densidad): `refreshMs` 1000 (como `client.refresh_ms` de la TUI; solo se pregunta con
  la vista abierta y la pestaña visible), `eventPageSize` 200 (`limit` de `event.list`),
  `eventKeep` 1000 (eventos que quedan en pantalla), `maxValueLength` 240,
  `startTimeoutMs` 120000 (un motor arranca su CLI antes de que `execution.start`
  responda) y `agentTimeoutMs` 600000 (un turno de agente). `RpcClient.request` acepta
  ese tiempo por llamada y sus errores llevan `code`.
- **Pruebas:** `pnpm test` (vitest) cubre la lógica pura; `src/operate/live.test.ts`
  corre las mismas llamadas contra un core real si hay `REGULUS_RPC_URL` (greeting,
  Ralph hasta `self.done`, `agent.prompt` → `pong`). El mock imita los métodos con datos
  de `mock/operate.json`.

---

## 9. Canvas editable (2026-11-08)

✅ **Hecho.** La vista Canvas edita el perfil entero y lo guarda en el core; cada acción es
un método que la CLI también llama. **Qué se edita:** perfil nuevo (nombre → id, un
workflow principal vacío); nodos desde la paleta (los tipos de `module.manifest`, por
módulo y con su estado de `module.list`; se arrastran o se pulsan, con los puertos del
manifiesto y los defaults de sus campos como `config`); mover, seleccionar varios y borrar
(Supr o botón); cables de salida a entrada solo si coinciden capacidad y tipo (`uses` con
`uses`, `then` con `then`) y la entrada tiene sitio (`cardinality`), si no se dice por qué;
el inspector edita nombre, cada campo según su tipo (texto, número, `select`, `map`,
`workflow`; lo desconocido como texto crudo) con su default y "reset", el workflow interno
(una caja) y, con el workflow elegido, sus puertos propios (`self`). Workflows extra se
crean y se borran (el principal no). Un nodo de un módulo ausente sale *no instalado* y se
sigue editando. **Cómo guarda:** un documento único en el cliente (`composition/editor/`,
funciones puras con tests); Guardar (`Ctrl/Cmd+S`) manda `profile.save` (sin `force` si es
nuevo) y enseña los `diagnostics` o el error del core tal cual; Comprobar (`profile.check`,
sobre lo guardado) marca los nodos que nombra; Ejecutar (`execution.start`) dice la
ejecución y los nodos desactivados; Borrar (`profile.delete`) pregunta antes. Con cambios
sin guardar, cambiar de perfil pregunta. **Ajustes** (en el dispositivo, botón de ajustes
del canvas): zoom mínimo y máximo (0.15 / 1.8), rejilla y ajuste a ella (16 px, apagado),
ancho de nodo (250), separación al añadir (40), agrupación de la paleta (por módulo),
autoguardado (apagado, 30 000 ms tras el último cambio), teclas de borrar (`Delete`,
`Backspace`) y de guardar (`s`). La vista "Todos los workflows" es de solo lectura: pulsar
un nodo abre su workflow.

---

## 10. Ajustes: secretos, servicio y módulos (2026-11-08)

✅ **Hecho.** Ajustes gana lo que la CLI ya hacía y la ventana no (`src/settings/`: lógica
pura en `logic.ts` con tests, componentes aparte). Ningún método nuevo en el core.

- **Módulos.** `module.list` con el estado de primer nivel (§3) de cada uno, icono y tono
  de `08-design-system.md`, y la razón: *desactivado* dice qué entrada falta, *roto* la
  razón del core, *incompatible* el protocolo que pide, *sin usar* es `idle` con
  `last_used` vacío. Primero lo que no funciona. Recargar no: el core no tiene
  `module.reload`; un módulo roto pide reiniciar el core.
- **Secretos.** `secret.list` (solo nombres), `secret.set` (`{name, value}`; el valor
  entra y no se vuelve a ver, avisa si reemplaza) y `secret.delete` con confirmación.
- **Servicio** (módulo `service`, `13-blocks.md` §3). Llaves: `service.key.list`,
  `service.key.new` (`{scope, name, policy}`; la llave en claro se ve una vez, con
  copiar) y `service.key.revoke` con confirmación. `service.endpoints` (dónde escucha
  cada motor vivo) y `service.log` con filtros de llave, workflow y `last` (al core) y
  de estado 2xx/4xx/5xx (en el cliente). Sin el módulo: *no instalado* (`-32601`) o *no
  disponible* con la razón del core (`-32002`), decidido por el código de error, nunca
  por el id del módulo.
- **Mock:** `mock/settings.mjs` + `mock/settings.json` contestan `secret.*` y
  `service.*` y añaden módulos en cada estado (activo, roto, desactivado, incompatible).

---

## 11. `[client]` de `config.toml` (2026-11-08)

✅ **HECHO** (2026-11-08) — `[client]` de `<home>/config.toml` es el único sitio del que los
clientes (el shell de escritorio, la app y la TUI) leen su configuración. El core la lee al
arrancar y la sirve con `config.client` (protocolo 1.13,
[`composition-preview.md`](composition-preview.md)); la TUI y el shell, que viven junto al
core, la leen del fichero.

| Clave | Por defecto | Quién la lee | Qué hace |
|---|---|---|---|
| `autostart` | `true` | shell, TUI | Lanzar un core si nadie responde; si no, se muestra el comando (§4) |
| `core_command` | vacío | shell, TUI | Con qué se lanza; vacío = `regulus` junto al cliente, si no el del `PATH` |
| `start_timeout_ms` | 5000 | shell, TUI | Cuánto tiene el core lanzado para escribir su puerto |
| `intro` | `true` | TUI | La intro (§7). La app no tiene intro: no la usa |
| `refresh_ms` | 1000 | TUI, app | Cada cuánto se pregunta al core qué corre |
| `start_view` | la página de inicio de la app | app | La vista con la que abre: `home`, `canvas`, `operate`, `templates` o `settings`; otro valor no cambia nada |
| `keys.*` | §7 | TUI | Teclas por acción: `save = "ctrl+w"` o una lista |
| `tui.*` | §7 | TUI; `event_page`, `event_keep` y `value_max` también la app | Números: `event_page`, `event_keep`, `value_max`, `cascade_step`, `node_width`, `node_height` |

- **Por proyecto.** El `[client]` del `regulus.toml` de un proyecto
  ([`11-composition.md`](11-composition.md) §13) va encima del global, clave a clave; las
  tablas (`keys`, `tui`) también se mezclan clave a clave. La app lo pide de nuevo al
  cambiar de proyecto; la TUI aún no elige proyecto y usa el global.
- **Precedencia:** lo que la persona cambia en la app (guardado en el dispositivo) > el
  proyecto > `config.toml` > los defaults de cada cliente. *Reset* en la app borra solo lo
  cambiado en ella: vuelve a lo del fichero.
- **`config.client` da solo lo escrito**, no los defaults, y `sources` dice de qué fichero
  sale cada valor (`config.toml` o `regulus.toml`), por `clave` o `tabla.clave`. Así la app
  distingue un valor del fichero de su propio default y lo dice junto al campo ("From
  config.toml"). Un proyecto que ya no está abierto cae al global.
- **Cuándo se lee.** El core lee el `[client]` global una vez, al arrancar: cambiarlo pide
  reiniciar el core. El del proyecto se relee en vivo (§13 de `11-composition.md`). La app
  pide `config.client` al conectar (también al reconectar) y al cambiar de proyecto; un fallo
  que no sea `-32010` (proyecto cerrado: cae al global) deja los valores que tenía.
- **Límites de la app.** La app recorta cada número a sus propios límites, que no son los de
  la TUI: `tui.event_page = 5` se ve como 10 en la app (con "From config.toml") y como 5 en
  la TUI.
- **`start_view` se aplica una vez,** con la primera respuesta tras abrir la app: la del
  proyecto que estaba elegido si lo había, si no la global, y solo si la persona no eligió
  página de inicio en Ajustes (una guardada antes de existir `start_view` cuenta como
  elegida) ni se movió aún. Cambiar de proyecto después no cambia de vista.
- **Nada rígido:** el core no decide qué entiende un cliente. Una clave que no conoce se
  conserva, se sirve tal cual y se avisa ("kept for clients", en `warnings` de
  `config.client` y al arrancar). Lo que la TUI no entiende en `keys` o `tui` (una acción
  que no existe, un número < 1 donde no vale, dos acciones con la misma tecla) la para al
  arrancar, antes de tomar la terminal, con el fichero y la clave: `…/config.toml: [client] tui.event_pag: no such setting`.
- **Código:** `ClientConfig` y `layer_client` en `regulus-core/src/config.rs`;
  `tui::client_table` y `tui::options` en la CLI; `src/core/clientConfig.ts`,
  `src/operate/settings.ts` y `applyStartView` (`src/store/useRegulusUi.ts`) en la app; el
  mock lo sirve desde `mock/settings.json`.
