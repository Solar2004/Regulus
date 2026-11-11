# 04 — El cliente (app y CLI)

> **v1.0** · 2026-10-05 · Lo vigente del cliente, rescatado de la versión anterior
> ([`old/04-client.md`](old/04-client.md), modelo antiguo: 8 métodos, agente en el centro).
> El contrato de métodos actual está en [`composition-preview.md`](composition-preview.md).
> Revisado el 2026-11-11 (candidato 0.2.1, sin publicar): campos `code` con CodeMirror (§9),
> módulos locales recargables (§10) y editar nodos propios en la app (§13).

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
5. **Bloqueado** (2026-11-08) — módulo local de un proyecto sin confianza (`blocked`,
   `03-modules.md` §6). No es un error: se dice el proyecto y se ofrece confiar en él.

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
- La conversación con un agente la guarda la caja Agente (`agent.history`, 2026-11-09;
  abajo, *Conversación*). Las ejecuciones necesitan `profile_id` (protocolo 1.4) para
  saber sus nodos.

**Segunda versión (2026-11-08).** La TUI hace lo que la app (§8, §9), con los mismos
métodos; ninguno nuevo en el core.

- **Editar** (`e` sobre un perfil, `n` para uno nuevo: nombre → id). Tres paneles: el árbol
  workflow → nodos; el inspector (nombre, cada campo del manifiesto según su tipo —texto,
  número, `select`/bool, `workflow`, `map` como JSON, lo desconocido como texto crudo; un
  `code` también sale crudo, en una línea, y `multiline` se ignora todavía— con
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
  `node_width` 250, `node_height` 160, `conversation_keep` 5000. Se cambian en
  `[client.keys]` y `[client.tui]` de `config.toml` (§11).

**Conversación (2026-11-09).** Lo que el panel *Conversación* de la app (§8) hace, en
Operar y en la CLI, con las mismas reglas y la lógica pura en un solo sitio
(`crates/regulus-cli/src/conversation.rs`, con tests); ningún método nuevo en el core.

- **Agente directo** (un nodo con entrada "usa" `agent.engine`): al elegirlo en *Nodes*, el
  panel de la derecha es su historial (`agent.history` desde `since` 0 y luego con `next`,
  páginas de `event_page`, fundido por `seq`; se lee también con el nodo parado). Enter
  escribe; enviar es `agent.send`, y el texto sale *queued* hasta que el historial trae la
  entrada `user` de ese turno. Arriba, la línea de sesión (`engine_session_id`, si se
  reanudó, estado).
- **Workflow enlazado**: `c` (`talk`) sobre una ejecución cuyo workflow tiene entradas y
  salidas propias "luego" `text@` abre el selector (↑↓ en una columna, ←→ entre entradas y
  salidas, Enter enlaza, `x` olvida, Esc). **Nunca se enlaza solo**, ni con un único par. El
  enlace se guarda por perfil + workflow en `<home>/conversations.json` (la forma de
  `ownConversations` de la app: `{"perfil/workflow": {input, output}}`), el mismo fichero
  que usa `regulus talk`; `[[client.operate.conversations]]` del `config.toml` global da
  valores de solo lectura y lo elegido aquí gana. Con enlace, `c` lo muestra y, mostrado,
  `c` vuelve al selector. La transcripción son los eventos del bus que Operar ya pide:
  `self.<entrada>` es la persona, `self.<salida>` la respuesta, `agent.event@1` actividad
  del nodo que la emitió.
- **Tarjetas**, como en la app: `text` en una burbuja viva (*writing*) que la respuesta
  sustituye, `thought`, `tool_call` + `tool_update` (estado con icono y texto), `plan`,
  `permission_request` con cómo se resolvió, `turn_ended`, `state`, lo desconocido con su
  JSON compacto (`value_max`).
- **Permisos**: `a` (`answer`) toma la petición abierta más vieja, busca en `human.list`
  (sin filtro de ejecución: el decisor puede estar en otra; se pide solo mientras hay una
  abierta) la decisión cuyo `value.context` coincide en ejecución, nodo y `request_id`, y
  abre las opciones (más *Reject* si ninguna es de rechazo) → `human.answer`. Sin esa
  decisión lo dice. **Cancelar**: `Ctrl+X` (`cancel_turn`), también escribiendo:
  `agent.cancel` del agente, o de cada agente con actividad en el turno abierto del
  workflow.
- **`regulus talk <ejecución> [--node N | --in P --out P] [--yes] [--once TEXTO]`**: lo
  mismo en la línea de órdenes. Elige, por orden: los flags (y recuerda el enlace de
  `--in/--out`), el enlace guardado o de `config.client` (el del proyecto de la ejecución
  encima del global), el único agente, y, sin agentes, propone el único par de texto, que
  se confirma en la terminal o con `--yes`; con más de un candidato sale con error y la
  lista (`--node a, --node b`, `--in …`, `--out …`). Sin `--once` es un REPL: el texto del
  agente sale en una línea que crece, la actividad en líneas `·`, y una petición de permiso
  dice cómo contestarla: `/<opción>`, `/allow`, `/reject` (opcionalmente con el id de la
  petición). `/cancel`, `/history`, `/help`, `/quit`; `//texto` manda `/texto`. Ctrl-C
  cancela el turno en curso y el segundo sale; al acabar la entrada espera al turno en
  curso hasta `--timeout`. Con `--once` manda un mensaje, imprime solo la respuesta en
  stdout (la actividad en stderr) y sale con error si el turno falla (entrada `system`),
  si la ejecución para, si una entrega falla con el bus ya parado, o pasado `--timeout`.
  Números: `client.refresh_ms` para preguntar, `tui.event_page`, `tui.value_max` y
  `tui.conversation_keep`, del mismo `[client]` que la TUI.
- **Límites:** la TUI usa el `[client]` global y no el del proyecto. Una caja con
  `history = no` no guarda nada: su conversación sale vacía y `--once` espera hasta
  `--timeout`. En un workflow, los turnos se agrupan por orden, no por correlación (como en
  la app), y cancelar llega a los agentes que emitieron actividad desde el último turno.
  Los comandos `/…` del REPL son fijos, no configurables. Un agente dentro de una caja no
  sale en la lista de *Nodes*; `regulus talk --node caja/agente` sí lo alcanza.
- **Nodos propios (2026-11-11):** la TUI no edita la fuente de un módulo ni lo recarga (eso
  es CLI: `regulus node source|save|check`, `project reload`, `profile sync`); no dibuja los
  campos `code` (sale texto crudo de una línea) ni oculta los campos con `when`. Al
  crear un nodo usa la misma `regulus_proto::shape::shape` que el core, con los defaults,
  así que un tipo con puertos dinámicos nace con los suyos; no vuelve a resolverlos si
  luego se edita un campo que alimenta la forma (para eso, `regulus profile check` y
  `profile sync`; `18-custom-nodes.md` §2.2).

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
  entrada `agent.engine` se habla en el panel *Conversación* (abajo); uno con entrada
  `participant`, el canal (`comm.history`, `comm.participants`,
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
  responda), `agentTimeoutMs` 600000 (un turno de `agent.prompt`; la conversación ya no lo
  usa) y `conversationKeep` 5000 (registros que una conversación guarda en pantalla). `RpcClient.request` acepta
  ese tiempo por llamada y sus errores llevan `code`.
- **Paneles de una ejecución como registro** (2026-11-09, `src/operate/panels.ts`): un
  array tipado con `id`, título, columna por defecto (`main` | `aside`) y una regla
  `isAvailable` (*Enviar* solo si el workflow tiene entradas propias; *Árbol* solo si la
  ejecución tiene padre o hijos; *Conversación* solo si hay con quién hablar). Son `nodes`,
  `conversation`, `send`, `events`, `tree`, `node` (el nodo elegido: su estado y su canal;
  un agente remite a *Conversación* si ese panel se ve, y si no la enseña ahí) y
  `decisions`. El orden, la columna y qué se ve se
  cambian en *View settings* (subir, bajar, columna, casilla) y se guardan en el
  dispositivo como los números; *Reset my changes* los quita. Si `config.client` trae
  `operate.panels` (`[client.operate] panels = ["tree", "events", { id = "node", place =
  "main" }]`), es el valor por defecto: lo que nombra se ve en ese orden, lo demás queda
  oculto, y el ajuste dice de qué fichero viene; el cambio de la persona gana. Sin
  framework de plugins: un panel nuevo es una entrada más del array.
- **Conversación** (2026-11-09, `src/operate/Conversation.tsx`; lógica pura con tests en
  `conversation.ts`). Panel `conversation` (columna principal por defecto) con un selector
  *Hablar con* cuando hay más de un sujeto. Se elige por capacidad, nunca por módulo:
  - **Agente directo**: un nodo con entrada "usa" `agent.engine`. Siempre se puede hablar
    con él, sin enlazar nada. El historial es `agent.history` (`12-agents.md` §5): al abrir
    el panel, y tras una reconexión o una recarga, se lee desde `since` 0 y se rehace; luego
    se sigue con `next` cada `refreshMs`, en páginas de `eventPageSize` (hasta 10 por
    vuelta), y se funde por `seq`, así que releer no duplica nada. Enviar es `agent.send`
    (no espera al turno): el texto sale *En cola* hasta que el historial trae la entrada
    `user` con ese `turn`. *Cancelar turno* (`agent.cancel`) aparece mientras la sesión está
    `busy`, hay algo en cola o un turno sin respuesta. Arriba, la línea de sesión:
    `engine_session_id`, si el CLI la reanudó (`native_resume`) y el estado con icono y
    texto. De un nodo parado se lee el historial (`stopped`) y no se escribe.
  - **Workflow enlazado**: un workflow es candidato si tiene al menos una entrada propia
    "luego" `text@` y una salida propia "luego" `text@`. **Nunca se presenta como
    conversación por tener esos puertos**: el panel enseña los candidatos y la persona
    elige entrada y salida una vez por perfil + workflow (*Talk to this workflow*); se
    guarda en el dispositivo (`ownConversations`, como los números) y se cambia u olvida
    desde el panel; *Reset my changes* no lo toca. También puede venir, de solo lectura, de
    `config.client`: `[[client.operate.conversations]]` con `profile`, `workflow`, `input`
    y `output`; lo elegido en el dispositivo gana, y un enlace cuyos puertos ya no existen
    no cuenta. Con el enlace, la transcripción sale de `event.list` de la ejecución
    (cursor `since`/`next`, sin persistir nada más que el enlace): lo emitido desde
    `self.<entrada>` es un turno de la persona (lo que registra `workflow.send`), lo
    emitido desde `self.<salida>` una respuesta, y lo emitido con capacidad
    `agent.event@1` (la salida `activity` de las cajas Agente de dentro) es actividad de
    ese nodo. La actividad cuelga del último turno de la persona anterior por `seq`.
    Enviar es `workflow.send` en la entrada enlazada. Debajo, las decisiones de `human.list`
    de la ejecución que no están detrás de una tarjeta.
  - **Tarjetas**, iguales en los dos modos: los `text` de un turno se juntan en una burbuja
    viva por nodo (*Writing*) hasta que llega la respuesta (`agent` o `system`, o la
    salida enlazada), que la sustituye; `thought` plegado (los seguidos, juntos);
    `tool_call` + sus `tool_update` en una tarjeta con título, `tool_kind`, estado (icono +
    texto) y el contenido desplegable; `plan` (el último sustituye al anterior);
    `permission_request` con `permission_resolved` (opción elegida o *Cancelled*);
    `turn_ended` con su `stop_reason`; `state`; un `kind` desconocido sale con su JSON.
  - **Permisos**: si `human.list` tiene una decisión de tipo `decision` cuyo
    `value.context` coincide en `execution_id`, `node_id` y `request_id`, la tarjeta ofrece
    cada opción (`human.answer {id, decision: "approve", fields: {option}}`) y, si ninguna
    es de rechazo, *Reject* (`decision: "reject"`). Sin esa decisión dice que espera
    respuesta. `human.list` se pide sin filtro de ejecución (el decisor puede estar en otra)
    y, en un agente directo, solo mientras hay una petición sin resolver.
  - El panel *Waiting for you* rellena el campo `option` con un selector (lo que el
    formulario limita con `enum`).
  - **Límites:** la caché del cliente (`session.ts`) solo dibuja lo último al volver; la
    verdad es el core. En un workflow con varios agentes, sus textos vivos desaparecen al
    llegar la respuesta del workflow (quedan en *Events*). Los turnos de un workflow se
    agrupan por orden, no por correlación: una respuesta tardía cae en el turno siguiente.
- **Árbol de invocaciones** (2026-11-09, `src/operate/tree.ts`): de la ejecución elegida,
  `execution.tree {execution_id}` (lee la raíz en `execution` o en `root`) cada
  `refreshMs`; contra un core sin el método (`-32601`) se arma con el campo `parent` de
  `execution.list` y lo dice. Cada fila: id, workflow, estado (icono + texto), nodo que la
  llamó (`caller`) y `tag`; los hijos llevan *Select* y, vivos, *Stop*. Los hijos siguen
  saliendo también, planos, en la lista de ejecuciones. El mock arranca como hijo el
  workflow que nombra un campo de tipo `workflow` de un nodo, y parar el padre para los
  hijos.
- **Pruebas:** `pnpm test` (vitest) cubre la lógica pura; `src/operate/live.test.ts`
  corre las mismas llamadas contra un core real si hay `REGULUS_RPC_URL` (greeting,
  Ralph hasta `self.done`, `agent.prompt` → `pong`). El mock imita los métodos con datos
  de `mock/operate.json`; `agent.send`, `agent.history` y `agent.cancel` llevan un turno
  guionizado que avanza un paso por cada `agent.history` del nodo o `event.list` de su
  ejecución (pensamiento, texto, `tool_call` + `tool_update`, plan, una petición de permiso
  que espera en `human.list` como decisión si la plantilla `chat` cablea su decisor, más
  texto, `turn_ended`), y un `workflow.send` que llega al `prompt` de un agente es un turno
  suyo, con `activity` y la respuesta en el bus.

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
`workflow`, `list` y, desde el 2026-11-10, `code` y texto `multiline`: el `code` con un
editor de código desde 0.2.1 y el `multiline` como área de texto; lo desconocido como texto
crudo) con su default y "reset", los campos con `when` ocultos mientras no se cumplan,
el workflow interno
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

**Campos de código y de varias líneas** (2026-11-10 el área de texto; 2026-11-11 el editor,
`composition/editor/fields.ts`, `Inspector.tsx`, `components/code/`). `type = "code"` se edita
con **CodeMirror 6**, cargado solo al abrir un campo `code` o una fuente de módulo (si el
trozo no carga, vuelve el `<textarea>` de antes); `type = "text"` con `multiline = true` sigue
siendo un área de texto proporcional que ajusta. Las pistas `label`, `description`, `language`
e `indent` del manifiesto (`03-modules.md` §1.3) dan nombre, ayuda, lenguaje junto al tipo y
lo que mete Tab. El editor resalta Python y marca los errores de sintaxis sin ejecutar nada.
**El texto vive en el editor** (borrador local, sin rehacer el grafo en cada tecla) y se
entrega al perfil tras una pausa, al perder el foco o al vaciar: Guardar, Comprobar,
Ejecutar, cambiar de perfil y aplicar una plantilla vacían primero. La primera tecla ya marca
el perfil como sin guardar. Deshacer/rehacer son los del editor; en un `code`, Tab sangra,
Shift+Tab quita sangría y **Esc y luego Tab sale del campo**. Probado en Chromium contra un
core real y medido por tecla: los números (el objetivo de 16 ms **no** se cumple en el p90)
y los límites están en `18-custom-nodes.md` §4; no se repiten aquí.

**Cajas en el canvas** (2026-11-09, `composition/editor/document.ts`, funciones puras con
tests):

- **Abrir caja.** Doble clic en una caja, *Open box* en el inspector o en el menú del
  botón derecho entra en su workflow; una miga de pan (workflow de la pestaña → cajas
  abiertas) vuelve a cualquier nivel. Cambiar de pestaña empieza la miga de cero; si el
  workflow en pantalla desaparece, se vuelve por la miga.
- **Crear caja desde la selección.** Con uno o más nodos elegidos (inspector de selección,
  inspector de nodo o menú del botón derecho) se pide un nombre (enseña el id que tendrá).
  Los nodos y los cables entre ellos pasan a un workflow nuevo; cada cable que cruzaba la
  selección se vuelve un puerto del workflow con el tipo del cable (`then` o `uses`) y la
  capacidad del puerto de dentro: entrada si venía de fuera, salida si salía. Un puerto
  por extremo interior (nodo, puerto): varios cables de fuera a la misma entrada la
  comparten (`many`), y una salida de dentro que iba a varios nodos es una salida cableada
  a todos. Dentro, los cables cortados van a `self.<puerto>`; fuera, a `<caja>.<puerto>`
  con su id de antes. Una caja dentro de la selección se mueve tal cual. Los ids que
  chocan (workflow, nodo caja, puertos) toman el siguiente libre.
- **Puertos de un workflow con los dos tipos.** El inspector del workflow añade entradas
  y salidas `then` o `uses` y edita id, capacidad (sugiere las que ya usan manifiestos y
  perfil), dirección, tipo, cuántos cables, `required` y `lazy` (solo `uses`). Cambiar
  dirección o tipo quita los cables que llegaban, y se avisa. El canvas acepta cables
  `uses` de una entrada `self` a entradas de dentro y de salidas de dentro a una salida
  `self` (dentro, una entrada del workflow hace de salida). La paleta sigue listando como
  caja todo workflow con puertos, también los `uses`.
- **Reemplazar dependencia.** El inspector de un nodo lista sus entradas `uses` con quién
  las sirve; un selector ofrece cada nodo del workflow con una salida `uses` de la misma
  capacidad y las entradas `uses` propias del workflow. Elegir uno quita ese cable y pone
  el nuevo; una entrada sin cable (o de `many`) se conecta igual.
- **Guardar como plantilla.** Botón de la barra (perfil ya guardado) → nombre,
  descripción, *solo este workflow* (el de la pestaña, como principal), variables
  (nombre, descripción, valor por defecto; sin defecto es obligatoria), fichero opcional y
  *reemplazar* (`force`) → `template.save`. Enseña la ruta escrita o el error por código
  (`-32005` inválida, `-32007` ya existe, `-32601` core sin el método, `-32602`) con el
  mensaje del core, y enlaza a la vista Plantillas. Con cambios sin guardar avisa de que
  la plantilla sale de la versión del core. El mock lo imita (con *solo este workflow*,
  el perfil lleva ese workflow como principal y los que coloca como cajas).

---

## 10. Ajustes: secretos, servicio y módulos (2026-11-08)

✅ **Hecho.** Ajustes gana lo que la CLI ya hacía y la ventana no (`src/settings/`: lógica
pura en `logic.ts` con tests, componentes aparte). Ningún método nuevo en el core.

- **Módulos.** `module.list` con el estado de primer nivel (§3) de cada uno, icono y tono
  de `08-design-system.md`, y la razón: *desactivado* dice qué entrada falta, *roto* la
  razón del core, *incompatible* el protocolo que pide, *sin usar* es `idle` con
  `last_used` vacío. Primero lo que no funciona. **Recargar** (desde 0.2.1, protocolo 1.17):
  los módulos **locales** de un proyecto tienen el botón *Reload module* (`project.reload`,
  §13) y *Edit source*; un módulo enlazado roto sigue pidiendo reiniciar el core.
  **Bloqueado** (2026-11-08): un
  módulo local de un proyecto sin confianza sale con su propio estado (`ShieldOff`, aviso),
  la razón del core y el botón **Trust <proyecto>**, que llama a `project.trust` con la
  carpeta que `project.list` da para el `project` de la entrada (protocolo 1.15); al
  volver, la lista se pide otra vez y el módulo aparece arrancado. Si el core no lista ese
  proyecto, se enseña el comando `regulus project trust <carpeta>`. Es lo que hace la CLI,
  sin método nuevo en el core: solo el campo `project` en `module.list`.
- **Secretos.** `secret.list` (solo nombres), `secret.set` (`{name, value}`; el valor
  entra y no se vuelve a ver, avisa si reemplaza) y `secret.delete` con confirmación.
- **Servicio** (módulo `service`, `13-blocks.md` §3). Llaves: `service.key.list`,
  `service.key.new` (`{scope, name, policy}`; la llave en claro se ve una vez, con
  copiar) y `service.key.revoke` con confirmación. `service.endpoints` (dónde escucha
  cada motor vivo) y `service.log` con filtros de llave, workflow y `last` (al core) y
  de estado 2xx/4xx/5xx (en el cliente). Sin el módulo: *no instalado* (`-32601`) o *no
  disponible* con la razón del core (`-32002`), decidido por el código de error, nunca
  por el id del módulo.
- **Actualizaciones** (2026-11-08, `06-stack.md` §5.4). General → *Updates*: la versión
  que corre, la del canal y el botón que la instala y reinicia la app. No pasa por el core:
  son comandos del shell (`update_settings`, `update_check`, `update_install`) sobre
  `tauri-plugin-updater`, con `[update]` del mismo `config.toml` que lee `regulus update`.
  Sin `url` dice "apagado" y qué fichero editar; sin `public_key`, que no se instala nada;
  en el navegador (mock), que es cosa de la app de escritorio o de `regulus update`. Con
  `check_on_start` la app mira al abrir y Ajustes lo enseña. Lógica en
  `src/settings/update.ts` con tests.
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
| `tui.*` | §7 | TUI y `regulus talk`; `event_page`, `event_keep` y `value_max` también la app | Números: `event_page`, `event_keep`, `value_max`, `cascade_step`, `node_width`, `node_height`, `conversation_keep` |
| `operate.conversations` | ninguno | app, TUI, `regulus talk` | Enlaces de solo lectura de un workflow como conversación: `[[client.operate.conversations]]` con `profile`, `workflow`, `input`, `output` (§7, §8) |

- **`[update]`** no es de `[client]`: lo leen la CLI (`regulus update`) y el shell de la app
  del mismo `config.toml`. Sus claves (`url`, `channel`, `check_on_start`, `public_key`,
  `timeout_ms`) están en [`06-stack.md`](06-stack.md) §5.4.
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
  elegida solo si no es la de por defecto) ni se movió aún. Cambiar de proyecto después no
  cambia de vista.
- **Nada rígido:** el core no decide qué entiende un cliente. Una clave que no conoce se
  conserva, se sirve tal cual y se avisa ("kept for clients", en `warnings` de
  `config.client` y al arrancar). Lo que la TUI no entiende en `keys` o `tui` (una acción
  que no existe, un número < 1 donde no vale, dos acciones con la misma tecla) la para al
  arrancar, antes de tomar la terminal, con el fichero y la clave: `…/config.toml: [client] tui.event_pag: no such setting`.
- **Código:** `ClientConfig` y `layer_client` en `regulus-core/src/config.rs`;
  `tui::client_table` y `tui::options` en la CLI; `src/core/clientConfig.ts`,
  `src/operate/settings.ts` y `applyStartView` (`src/store/useRegulusUi.ts`) en la app; el
  mock lo sirve desde `mock/settings.json`.

## 12. Regulus Code (2026-11-10)

El canvas ofrece **Arrange workflow**, **Arrange selection** y **Undo arrange**. Ordena
por conexiones usando los tamaños medidos; ciclos y grupos desconectados se resuelven de
forma determinista. Una selección conserva las posiciones de los demás nodos, que cuentan
como obstáculos. Ajustes del canvas permite cambiar dirección, separación y altura de
respaldo. La vista conjunta permanece de solo lectura. Insertar por clic o arrastrar busca
espacio libre localmente. Deshacer restaura solo las posiciones y deja de ofrecerse si los
nodos ordenados se han movido o eliminado después.

✅ **HECHO** — `regulus code [carpeta]` abre una pantalla fullscreen propia: conversación del
Agente, composer, permisos, archivos, cambios, actividad y trabajadores. `--once`,
`--detach` y los comandos `code inspect` siguen siendo aptos para scripts. La vista Code de
Tauri ofrece el mismo flujo con selector de carpeta, proveedor/modelo, política de permisos,
menú `+` de contexto y acceso al canvas para editar el harness. La política `allow` se muestra
como auto-aceptar cambios y comandos: no convierte la shell en sandbox y conserva los derechos
del usuario del core. La conversación ocupa la vista principal; `[client.code] setup = "always"`
muestra el formulario antes de crear un perfil y `setup = "needed"` lo muestra cuando falte
un requisito. `status_bar` controla la fila de contexto, `status_fields` elige y ordena sus
campos (`root`, `model`, `branch`, `state`, `session`), y `side_panel` decide si el panel de
espacio de trabajo empieza abierto (por defecto, cerrado). La tecla configurada para el panel
y `/attach <file>` permiten abrirlo o adjuntar un archivo al borrador sin enviarlo.

Al escribir `/` en el composer, aparece un menú filtrable de comandos con descripción.
Las flechas eligen, Tab (`keys.code_complete`) completa y Escape cierra; Enter completa una abreviatura y envía
un comando que ya está escrito completo. Escribir argumentos vuelve a la edición normal.
El catálogo de comandos alimenta también la ayuda y la ejecución; los ids de opciones de
permisos siguen siendo respuestas válidas. Los eventos de pegado de la terminal insertan
texto sin ejecutarlo; una terminal sin bracketed paste no permite distinguirlo de teclear.

```toml
[client.code]
status_bar = true
status_fields = ["root", "model", "branch", "state", "session"]
side_panel = false
setup = "needed"
```

---

## 13. Nodos propios en la app (2026-11-11)

✅ **Hecho** en el árbol de trabajo (candidato 0.2.1, **sin publicar**; el binario 0.2.0 habla
el protocolo 1.16 y no lo trae). Detalle del contrato, de la comprobación sin ejecutar y de
lo medido en [`18-custom-nodes.md`](18-custom-nodes.md); aquí, lo que hace el cliente
(`src/composition/authoring/`, `src/components/code/`, `src/settings/ModuleSettings.tsx`).
Ningún método es exclusivo de la app: cada acción tiene su comando de CLI (principio 5).

| Qué | Cómo |
|---|---|
| **Nodo nuevo** | Botón *New Python node* de la paleta → `node.init` en un proyecto abierto (`project.list`; el diálogo enseña la regla del id y los errores por código: `-32015` ya existe, `-32010` el proyecto no está abierto, `-32601` core antiguo). Sin proyectos (un hogar nuevo), o con *Open a project folder…*, abre una carpeta por su ruta (`project.open`, sin confiar; a una carpeta sin `regulus.toml` se le escribe uno) y la elige. Abre su fuente y refresca paleta y manifiestos |
| **Panel de fuente** | Junto al canvas, para el `.py` de **un** módulo local (`source.get`): editar, *Check* (`source.check`, sola tras `sourceCheckMs`), *Save* (`source.save` con la revisión leída), *Save & reload*, la lista de nodos leída de forma estática y «recarga para aplicar» si no coincide con el módulo cargado. `Ctrl/Cmd+S` dentro del editor guarda el **fichero**, no el perfil. Un fichero cambiado fuera da un aviso con *cargar el suyo*, *sobrescribir* o *seguir editando*. Un proyecto sin confianza muestra el módulo `blocked` y la acción *Trust project*. El borrador sin guardar se conserva al cambiar de vista; pedir otra fuente, cerrar el panel, recargar la página o cerrar la ventana con él pendiente pregunta antes (`18-custom-nodes.md` §4.2) |
| **Recargar** | *Reload* en el panel y en Ajustes → Módulos (para un módulo local; ahí también *Edit source*). Con ejecuciones vivas (`-32014`) un diálogo las lista: *mantener* (no cambia nada) o *parar N y recargar* (`stop_executions`). El resultado de cada módulo se dice con palabras (reemplazado, se queda el anterior, bloqueado…) y las ejecuciones paradas. Solo se releen módulos y manifiestos; los borradores no se tocan |
| **Forma y variantes** | Solo `module.shape` resuelve puertos (el cliente no lleva otro resolutor). La paleta añade un tipo con puertos dinámicos con la forma del core; el inspector oculta los `hidden_fields`, enseña los problemas de forma y, si los puertos del nodo no son los de su forma, un aviso con *Review* |
| **Sincronizar** | `profile.sync` sobre el borrador sin guardar, nunca escribe. Un diálogo lista puertos añadidos, quitados y cambiados y **cada cable que cae con su motivo**; el botón es destructivo si cae alguno. Aplicar es un solo cambio del documento con Deshacer; nada se guarda hasta Guardar. Una vista previa solo se aplica sobre el documento para el que se pidió: si ya es otro perfil se descarta, y si el documento cambió desde entonces se pide una nueva. Sincroniza sola solo cuando el usuario edita un campo del que depende la forma **y** no cae ningún cable; tras abrir un perfil o recargar espera a *Review* |
| **Core antiguo** | `-32601` → mensajes claros; sin *Edit source* ni aviso de forma; *New Python node* dice que necesita 1.17. Sin errores de consola (comprobado contra un core 0.2.0) |

**Ajustes del dispositivo** (botón de ajustes del canvas, con los de §9; nada va a
`config.toml`):

| Clave | Por defecto | Rango | Qué |
|---|---|---|---|
| `codeCommitMs` | `400` | 0-10000 | pausa tras la que un campo `code` entrega su texto al perfil (al instante al perder el foco o al guardar) |
| `codeLintMs` | `300` | 0-10000 | pausa tras la que el editor marca la sintaxis |
| `codeMinLines` · `codeMaxLines` | `4` · `18` | 1-50 · 4-200 | altura de un campo `code` antes de desplazarse |
| `sourceCheckMs` | `1500` | 0-60000 | pausa tras la que se pide `source.check`; `0` = solo a petición o al guardar |
| `shapeDelayMs` | `250` | 0-5000 | pausa tras cambiar un campo que alimenta la forma, antes de pedir `module.shape` |
| `reloadTimeoutMs` | `60000` | 1000-600000 | cuánto puede tardar una recarga o un nodo nuevo |
| `checkTimeoutMs` | `15000` | 1000-120000 | cuánto puede tardar comprobar o guardar una fuente |
| `sourceIndent` | `4` | 1-8 | espacios que mete Tab en una fuente |

**Lo que no hace, dicho sin adornos:** no abre un editor externo, no enseña el stderr de un
módulo que corre (sí el motivo de un fallo de arranque o de recarga), no avisa de un
`{{nombre}}` sin declarar, no es un IDE (un fichero por módulo, sin árbol ni git) y no edita
Rust. La TUI no tiene nada de esto (§7). **Rendimiento:** el objetivo de 16 ms por tecla
**no se cumple en el p90**; los números medidos están en
[`18-custom-nodes.md` §4.3](18-custom-nodes.md#43-probado-y-medido).
