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
  espera el puerto nuevo. Lo configura `[client]` de `config.toml`: `autostart` (`true`),
  `core_command` (vacío = `regulus` junto a la app, si no el del `PATH`) y
  `start_timeout_ms` (5000), más `intro` y `refresh_ms` para la TUI (§7). Lo que imprime el core lanzado va a `<home>/core.log`. En un
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
