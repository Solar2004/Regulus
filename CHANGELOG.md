# Changelog

Lo que cambia de cara a quien usa Regulus, agrupado por impacto. No es el `git log`: el
`git log` es para quien escribe el código, esto es para quien lo consume.

Formato basado en [Keep a Changelog](https://keepachangelog.com/es-ES/1.1.0/). Versionado:
[`docs/06-stack.md` §5](docs/06-stack.md) — tres ejes (app, protocolo, módulo).

**La entrada se escribe en el mismo commit que el cambio**, no al cortar el release
([`docs/09-git.md` §6](docs/09-git.md)).

---

## [No publicado]

### Añadido

- **Comandos sugeridos en la TUI Code:** `/` abre una lista filtrable con descripciones,
  navegación por flechas y autocompletado con Tab o Enter; ayuda y ejecución comparten
  catálogo. Los comandos pegados no se ejecutan hasta enviarlos.
- **Canvas ordenable:** ordenar workflow o selección con medidas reales, dirección y
  separación configurables, y deshacer el último ordenado. Insertar por clic o arrastrar
  busca espacio libre sin mover los demás nodos.
- **Regulus Code:** módulo de workspace y herramientas de programación, plantilla de
  harness editable, conversación en TUI y vista Code en la app. La TUI permite añadir
  archivos al borrador con `/attach`, configurar la barra de estado y abrir los paneles
  bajo demanda.
- **Mascota de Regulus Code:** león estelar original definido en una rejilla editable,
  con SVG generado, paleta compartida y expresiones para los clientes.

### Corregido

- Los clientes no ofrecen conversar con un nodo cuyo motor está reservado de forma
  perezosa para sus trabajadores; un harness con delegación conserva su único agente
  conversable en `regulus talk`, la TUI y Operar.

## [0.2.0] - 2026-11-09

### Añadido

- **Contribuir al proyecto:** licencia Apache-2.0 en `LICENSE` y metadatos de los paquetes,
  `CONTRIBUTING.md`, guías de entrada en inglés, tareas iniciales propuestas y descargas
  verificadas al principio del README. GIF real de 25 segundos de composición y
  conversación con el ejemplo Greeter; distingue desarrollo de la versión publicada.
- **Herramientas MCP en el motor nativo** (`docs/12-agents.md` §8): monta los toolsets
  `mcp-stdio` de la caja Agente, conserva las políticas y hooks y libera el proceso al
  parar la sesión. Comprobado escribiendo y leyendo memoria por el puente real.
- **Cajas que se cruzan** (`docs/02-architecture.md` §4, `docs/11-composition.md` §6-7,
  protocolo `1.16`): un cable "usa" entra o sale de una caja o workflow (se colapsa al
  aplanar; avisos `wire.uses_cycle` y `wire.boundary_cardinality`); un nodo mantiene
  ejecuciones hijas vivas con enlaces a lo que tiene cableado (`execution.tree`, `regulus
  exec tree`); un perfil se guarda como plantilla (`template.save`, `regulus template save`).
- **El agente cuenta lo que hace** (`docs/12-agents.md` §5): eventos de sesión (texto,
  pensamiento, herramientas, plan, permisos), permisos `ask` resueltos por un Decisor
  (`decision@1`, nodo `human.decider`, `regulus human answer`), conversación guardada
  (`agent.send`, `agent.history`, `agent.cancel`, salida `activity`, `regulus agent
  send|history|cancel`) y sesión nativa reanudable (`resume = native`).
- **Conversación en los tres clientes** (`docs/04-client.md` §7-8): panel Conversación en
  la app, vista en la TUI (`c`, `a`, Ctrl+X) y `regulus talk <ejecución> [--node | --in/--out]
  [--once]`. Un agente directo no pide nada; un workflow se enlaza una vez, a mano, y el
  enlace se guarda (`conversations.json`, `[client] operate.conversations`).
- **Workflows como herramienta, lote y trabajador** (`docs/13-blocks.md` §3,
  `docs/12-agents.md` §5.1): `flow.tool` (un workflow como `tool@1`, modos call y job),
  `flow.batch` (una hija por valor, recoge los resultados), módulo `delegation`
  (`delegate`: trabajadores vivos con su propio motor, `regulus workers
  ls|create|send|result|close`, plantilla `delegation`). `call`, `batch` y `tool` pasan a la
  hija lo que tienen cableado.
- **Documentos y RAG** (`docs/13-blocks.md` §3, §5): módulo `documents` (ingesta, troceo,
  recuperación léxica con motor intercambiable, contexto y puerta de citas: comprueba
  procedencia, no verdad), módulo `inference` (una llamada al modelo por valor), `regulus
  docs ls|query|rm`, plantillas `rag-chat` y `rag-eval`; `research` corre por lotes y busca
  en una colección de documentos.
- **Motor nativo** (`docs/12-agents.md` §8): el bucle del agente lo corre Regulus sin CLI
  detrás: módulo `engine-native`, nodo `native`, proveedor openai o anthropic, herramientas
  `tool@1` directas y hooks `hook@1` cableados que ponen contexto, deciden la política de
  herramientas y cuándo acaba el turno; plantilla `native-harness`.
- **Cajas en la app** (`docs/04-client.md` §8-9): abrir una caja, crear una desde la
  selección, puertos "usa" del workflow, sustituir una dependencia, guardar como plantilla;
  Operar enseña el árbol de llamadas.

### Corregido

- `rag-chat` con inferencia directa no arranca un agente de programación no seleccionado;
  la rama de agente se invoca como workflow y solo requiere su CLI al recibir una consulta.
- ACP conserva el diagnóstico de salida del agente cuando el cierre del transporte
  llega antes que la recogida de stderr; la sesión termina y los siguientes turnos fallan.
- **Historial independiente por ejecución** (`docs/12-agents.md` §5): dos trabajadores
  del mismo workflow no mezclan transcript, actividad ni referencia de sesión nativa.
  `conversation = named` permite continuar una conversación elegida; `workflow` accede
  al formato anterior. Un único nodo vivo puede poseer cada conversación reutilizable.
- `flow` declara el protocolo mínimo que necesitan sus nuevos nodos de lote y herramienta.

## [0.1.0] - 2026-11-08


### Añadido

- **Lógica de IA, el core** (`docs/13-blocks.md` §2, protocolo `1.5` → `1.8`): el cable
  "luego" lleva dato por un bus por ejecución (`event.list`, `regulus events --follow`,
  límites `[bus]`); un store SQLite global y por proyecto guarda ejecuciones, checkpoints
  y el diario del bus, y las ejecuciones sobreviven al reinicio de `serve` (`[store]
  resume`); un workflow declara puertos propios y se mete en otro como caja (`self.<puerto>`,
  `workflow.send`, `regulus send`); un nodo llama a un workflow como ejecución hija y
  espera su resultado (`Execution.parent`, `node/child`, límites `[call]`). Módulo `flow`
  con el nodo `call`. Ejemplos en `crates/modules/hello/examples/` y
  `crates/modules/flow/examples/call.toml`.
- **Hooks del agente** (`docs/13-blocks.md` §4, protocolo `1.9`): la caja Agente tiene
  la entrada `hooks`. Un nodo `rule` (módulo `hooks`) veta una herramienta o añade
  contexto en un evento (`pre_tool`, `stop`, `session_start`…). Con Claude Code en perfil
  aislado se escriben en su `settings.json` y llaman al puente `regulus hook`; el core los
  reenvía con `hook.dispatch` y espera como mucho `[hooks] timeout_ms`. opencode aún no.
  Ejemplo en `crates/modules/hooks/examples/guarded.toml`.
- **Agentes** (`docs/12-agents.md`): la caja Agente y los motores Claude Code y opencode,
  que hablan con su CLI por ACP. Cada motor detecta las instalaciones del PC
  (`claude.detect`, `opencode.detect`) y usa la que elijas con tu perfil
  (`config_dir`). Ejemplo en `crates/modules/agent/examples/agents.toml`.
- Ejecuciones: `execution.start` / `execution.stop` (protocolo `1.2`) y los comandos
  `regulus run`, `regulus stop`, `regulus ps` y `regulus agent prompt`. Un nodo al que
  le falta una entrada obligatoria queda desactivado sin tumbar el resto.
- El core: `regulus serve` sirve JSON-RPC 2.0 por WebSocket en loopback, protocolo `1.1`
  (`hello`, `profile.list`, `profile.get`, `profile.check`, `module.list`,
  `execution.list`). Pide el token de `<home>/token` (`Authorization: Bearer` o
  `?token=`) y un `hello` compatible antes de cualquier otro método. La UI funciona contra
  él con `VITE_RPC_URL=ws://127.0.0.1:<puerto>/?token=<token>`.
- `regulus serve --without <módulo>` arranca sin ese módulo; un módulo que falla o entra
  en pánico queda `broken` sin tumbar el resto.
- Comandos cliente: `regulus call`, `regulus module ls`, `regulus profile ls` y
  `regulus graph <perfil>`.
- Módulo `hello` (`hello.greet`) y matriz de ablación automática sobre los módulos
  instalados.
- Harness de git: convenciones de commit en `.gitconventions`, hooks en `.githooks/`,
  validador ejecutable a mano (`scripts/commit-lint.sh`), escáner de credenciales
  (`scripts/secrets-scan.sh`) e instalador (`scripts/install-hooks.sh`). Reglas en
  `docs/09-git.md`.
- `.gitignore` y plantilla de mensaje de commit (`.gitmessage`).
- **Lógica: `flow` y `files`** (`docs/13-blocks.md` §3.2): `flow` suma `switch`, `loop`,
  `join`, `fanout`, `wait`, `variable` y `convert`; el módulo nuevo `files` trae `read`,
  `write`, `document` (un documento que el agente ve cada turno, por su entrada `hooks`) y
  `watch`. Capacidades `text@1`, `json@1` y `path@1` en `regulus-proto`. Ejemplos en
  `crates/modules/flow/examples/logic.toml` y `crates/modules/files/examples/`.
- **Procesos y guardas** (`docs/13-blocks.md` §3.2): módulo `process` con `command`,
  `gate` (el código de salida decide y reenvía el mismo valor), `job` (log por ejecución,
  timeout) y `terminal` (PTY); `node/stop` mata el grupo de procesos sin dejar nada vivo.
  Módulo `guards` con `budget`, `retry`, `timeout`, `error` y `watchdog`, envolturas para
  cualquier cable "luego". Ejemplos en `crates/modules/process/examples/job.toml` y
  `crates/modules/guards/examples/backpressure.toml`.
- **Jueces y disparadores** (`docs/13-blocks.md` §3.2): módulo `judge` con Juez, Clasificador,
  Extractor y Rúbrica en árbol sobre un proveedor `openai` o `anthropic` (Ollama, LM Studio o
  nube) con schema estricto, reintento y salida `error`; el motor se cambia por cualquier
  `judge.engine@1`. Módulo `triggers` con Cron, Webhook (`regulus call triggers.webhooks`),
  Evento del bus, Chat con transcript (`triggers.transcript`), Inactividad y Umbral. Ejemplos en
  `crates/modules/judge/examples/judge.toml` y `crates/modules/triggers/examples/triggers.toml`.
- **Memoria** (`docs/13-blocks.md` §3.3): módulo `memory` con Memoria (`store`, entradas por
  nombre y ámbito proyecto o ejecución con topes, como herramientas `tool@1` para una caja
  Tools), Inyección por hook (`inject`, `hook@1`), Reflexión (`reflect`) y Biblioteca de skills
  (`library`, `SKILL.md` que carga `skills`); las entradas sobreviven al reinicio del core.
  `regulus call memory.entries` y `memory.tools`.
  Ejemplo en `crates/modules/memory/examples/memory.toml`.
- **Web** (`docs/13-blocks.md` §3.3): módulo `web` con HTTP, Fetch legible y paginado,
  Búsqueda sobre un motor `web.search@1` (de serie, SearXNG), PDF (`lopdf`) y Descarga; destinos
  privados rechazados tras DNS salvo `allow_private`, User-Agent fijo `regulus/<versión>`; una
  ruta de `pdf` formada por el valor no sale de la carpeta del nodo.
  Ejemplo en `crates/modules/web/examples/web.toml`.
- Módulo `human` (`docs/13-blocks.md` §3.3): **Aprobación** (`approval`: el valor espera bajo
  un id, formulario opcional como JSON schema, `merge_fields`, `timeout_ms` con salida
  `timeout`) y **Revisión** (`review`: el texto se edita antes de seguir). Se decide con
  `regulus human list|show|answer <id> approve|reject [--field k=v] [--text|--file]` o con un
  valor en `decide`. Lo pendiente vive en el store y sobrevive a reiniciar el core (también a
  `kill -9`). Ejemplo en `crates/modules/human/examples/approval.toml`.
- Módulo `service` (`docs/13-blocks.md` §3.3): un workflow con puertos propios se ofrece a
  sistemas de fuera. Llaves `rgk_` (`regulus service key new|list|revoke`) guardadas como
  SHA-256 con ámbito `<proyecto>[/<workflow>]`; políticas (cooldown, ventana, concurrencia,
  presupuesto) por llave, por workflow y por llave×workflow; log en el store (`regulus service
  log`); motor intercambiable `service.engine@1` con el HTTP de serie (síncrono y por job).
  `regulus service expose <workflow>` imprime los nodos. Ejemplo en
  `crates/modules/service/examples/expose.toml`. Nueva dependencia: `sha2`.
- `service` y `human`, tras revisión: el listener HTTP acota el tiempo total de escribir y de
  vaciar una conexión, y responde 503 sin bloquear el hilo que acepta; parar un endpoint libera
  lo que tenía en vuelo (plazas, presupuesto, jobs, log 503); `budget_requests` reserva las
  peticiones en vuelo; las 401 ya no van al log; una llamada síncrona que caduca pasa a job sin
  perder la respuesta. `human` borra lo pendiente solo cuando el valor ya salió y un plazo que
  venció con el core caído salta al reanudar.
- **Entorno: `workspace` y `tasks`** (`docs/13-blocks.md` §3.3): módulo `workspace` con una
  carpeta por ejecución con cuota (`quota_exceeded`), motor git worktree (rama propia,
  `.worktreeinclude`, merge y archivo) o carpeta simple sin git, o el `workspace.engine@1` que
  se enchufe; cola de merge que solo mergea workspaces propios (o de su `repo` y `root`) tras pasar sus gates, con el checkout de la base limpio; artefactos
  (`regulus call workspace.artifacts`). Módulo `tasks` con cola de tareas persistente con
  estados y dependencias, Reclamar con lock y lease (un lease vencido cuenta como intento; `max_attempts`, 3), y Ledger de hechos, plan y progreso que
  también se inyecta como hook (`tasks.list`, `tasks.ledger`). Ejemplos en
  `crates/modules/workspace/examples/worktree.toml` y `crates/modules/tasks/examples/queue.toml`.
- **Plantillas en el core** (`docs/11-composition.md` §7, protocolo `1.10`): un perfil TOML
  con huecos `{{nombre}}` y una tabla `[template]` (`name`, `description`, `requires` con
  versión semver, `vars` con `default` opcional). `template.list`, `template.get` y
  `template.apply` las leen de `[templates] paths` (por defecto `<home>/templates`) solo al
  pedirlas; aplicar sustituye literalmente, comprueba módulos y perfil, escribe
  `<id>.toml` en la primera ruta de `[profiles] paths` y lo carga sin reiniciar. Nueva
  dependencia: `semver`. CLI: `regulus template ls|show|apply <nombre|ruta> [--set k=v]
  [--id] [--force]`. Las plantillas del repo viven en `templates/`; la primera,
  `templates/greeting.toml`.
- **La caja Agente en cadenas "luego"** (`docs/12-agents.md` §5): entrada `prompt` (cola
  por nodo, `max_queue`, un prompt a la vez sin bloquear el bus), salidas `reply`,
  `failed` y `usage` (solo si el motor da uso), y el campo `fresh_context` (sesión nueva
  antes de cada prompt). El motor opencode gana el campo `model` (un proveedor conectado
  manda sobre él). Ejemplo en `crates/modules/agent/examples/then.toml`. Arreglo: el nodo
  `command` (y los demás de `process`) da al hijo `PWD` igual a su `cwd` salvo que `env`
  lo fije; antes heredaba el de `serve` y opencode trabajaba en otro directorio.
- **Plantilla Ralph** (`docs/13-blocks.md` §5, fila 2): `templates/ralph.toml`. Bucle de
  `max_turns` vueltas que relee `PROMPT.md`, lo junta con el último resultado del gate, se
  lo da a la caja Agente con `fresh_context = "yes"` (sesión nueva cada vuelta; motor
  `engine-opencode`, modelo `model`, por defecto `opencode/glm-5.3-flash-free`), añade la respuesta a
  `fix_plan.md` y corre `test_command` en `workdir`: código 0 acaba en `done`; si no, su
  salida es el feedback de la vuelta siguiente. Presupuesto por tiempo (`max_ms`).
  `regulus template apply ralph --set workdir=<repo>`.
- **La app puede editar** (`docs/11-composition.md` §7, protocolo `1.11`): `module.manifest`
  da el manifiesto de un módulo o de todos (tipos de nodo con puertos, campos, defaults y
  opciones); `profile.save` escribe un perfil entero en la primera ruta de `[profiles] paths`
  y lo carga sin reiniciar (pisar uno existente pide `force`); `profile.delete` lo borra si no
  está en uso. Errores nuevos `-32008` (perfil con errores) y `-32009` (perfil en uso). Al
  guardar se pierden los comentarios del fichero. CLI: `regulus module show`,
  `regulus profile save` y `regulus profile rm`. El mock de la UI los imita en memoria.
- **La app opera y aplica plantillas** (`docs/04-client.md` §8): vistas Operar (perfiles
  con Run, ejecuciones con Stop, nodos con su estado, eventos del bus, envío a las entradas
  del workflow, conversación con un agente, canal y decisiones de `human`) y Plantillas
  (lista, fichero, aplicar con formulario de variables, abrir en el canvas o ejecutar).
  Ajustes de la vista guardados en el dispositivo. `pnpm test` (vitest) en el frontend.
- **Canvas editable** (`docs/04-client.md` §9): en Composición se crea un perfil, se añaden
  nodos desde una paleta de manifiestos (buscable, con el estado del módulo), se mueven, se
  borran y se cablean (un cable que no encaja por capacidad, tipo o cardinalidad se rechaza
  con el motivo); el inspector edita nombre, campos del manifiesto, workflow interno y los
  puertos propios de un workflow. Guardar (`Ctrl/Cmd+S`), Comprobar, Ejecutar y Borrar
  llaman a `profile.save`, `profile.check`, `execution.start` y `profile.delete`. Ajustes
  del canvas (zoom, rejilla, ancho de nodo, agrupación, autoguardado) en el dispositivo. El
  mock suma `profile.check` y los diagnósticos de `profile.save`; `pnpm test` (vitest)
  prueba la lógica.
