# 13 — Bloques: la lógica de IA de serie y las plantillas

> **v1.0** · 2026-11-07 · ✅ **DECIDIDO por Alexander** (2026-11-07). Sale de la
> conversación del mismo día y de cinco investigaciones con fuentes (§8). Define qué
> piezas genéricas trae Regulus, qué pide del core, y qué sistemas conocidos se
> reconstruyen como plantilla. Manda sobre `plan.md` §3 en el orden de trabajo.

---

## 1. La idea, en palabras de Alexander

> *"No ceñirme a un diseño: una estructura para darle vida a lo que sea."* ·
> *"No existe nada permanente ni poco flexible: todo encaja en un módulo, es instalable,
> incluso un módulo que agregue compatibilidad entre dos cosas."* ·
> *"Lógica con redes neuronales."*

Regulus es **programación por flujo de datos con cajas, donde una IA es una caja más**.
Como ComfyUI o n8n, con dos cosas que ellos no tienen: cables permanentes ("usa": el
agente *usa* su proveedor y sus MCP) y cajas que se abren (`11-composition.md` §6).

Con eso se hace lo que Alexander describió: un agente custom (el CLI de Claude con o sin
su harness, system prompt, modelo de tal proveedor, scripts, MCPs, skills, un supervisor
que activa un MCP según el prompt), un workflow **chateable** que hace de CLI propio,
automatizaciones headless, workflows que se llaman entre sí, núcleos de decisión ("si pasa
esto, ejecuta tal script"), replicar un paper, o un flujo que devuelve una imagen en
blanco y al 50 % una generada con marca de agua por Gemini con rotación de claves.

✅ **Regla:** los sistemas conocidos (Ralph, Spec Kit, BMAD, ECC, Gas Town, Orca…) **no
son nodos**. Se extrae su lógica en piezas genéricas (§3) y cada sistema se reconstruye
como **plantilla** (§5). Un nodo "Spec Kit" sería endurecer la caja.

---

## 2. Lo que pide el core

El core sigue sin conceptos de IA (`02-architecture.md` §4). Gana solo lo que los módulos
de §3 piden, y cada pieza tiene quien la pide:

| # | Pieza | Quién la pide |
|---|---|---|
| 1 | **Cable "luego" con dato**, tipado por capacidad (`text@1`, `image@1`…), sobre el bus. `WireKind::Then` ya existe como tipo y el validador lo acepta (`regulus-proto/src/graph.rs`), pero no hay bus y ningún dato viaja entre cajas | `flow`, `triggers`, `process`, todo lo de §3 |
| 2 | ✅ *Construido* (2026-11-07, paso 1b). **Store** (SQLite, uno por proyecto y uno global, `02-architecture.md` §6): ejecuciones que sobreviven al reinicio, arrancan con `serve` y se reanudan desde checkpoint | `human` (pausar y reanudar), `tasks`, `memory`, bucles largos |
| 3 | ✅ *Construido* (2026-11-07, paso 1c). **Workflow como caja**: puertos propios de entrada y salida en un workflow del usuario (`self.<puerto>` dentro, nodo sin módulo con `internal_workflow_id` fuera, `workflow.send` / `regulus send` para hablarle; `02-architecture.md` §4 regla 4, §6). Ya existen `Node.internal_workflow` y `WorkflowKind::Module` (`11-composition.md` §6) y el "luego" que cruza de canvas (§5); lo nuevo es que un workflow declare sus puertos, se enchufe en otro y se le hable (`chat`) desde la TUI o la app | el "CLI custom", `Call` |
| 4 | ✅ *Construido* (2026-11-07, paso 1d). **Sub-ejecución "llama y espera"**: `parent` en `Execution`; `ModuleCtx::call_workflow` arranca una hija de un workflow con puertos y su primera salida vuelve al nodo como `node/child`; parar al padre para a las hijas (`02-architecture.md` §4 regla 5). El nodo `call` de `flow` lo usa: un valor, una hija. Que el agente lo use como herramienta queda para su paso | subagentes N, handoff, orquestador-workers, Roo `new_task` |

Lo que **no** entra en el core: hooks (ranura de la caja Agente, §4), disparadores,
scripts, jueces, memoria, workspaces, colas. Todo módulo, todo se quita uno a uno.

**Qué enmienda en otros docs** (como hizo `11-composition.md` §11; todas aplicadas):

- ✅ *Aplicada* (2026-11-07, paso 1a; el bus en `02-architecture.md` §6).
  `11-composition.md` §4 y `02-architecture.md` §4: el "luego" **lleva dato** tipado por
  capacidad; el bus es quien lo entrega. Sigue sin haber motor de flujo aparte del bus: un
  Bucle o un Switch son nodos de `flow` que reciben y emiten por "luego". El corte por
  profundidad configurable del core se mantiene como red de seguridad; el tope N del
  Bucle es del nodo. `Port.lazy` y `cardinality` ya existen y se usan tal cual.
- `02-architecture.md` §4: `Workflow` gana puertos propios ✅ (*aplicada* 2026-11-07,
  paso 1c) y `Execution` gana `parent` ✅ (*aplicada* 2026-11-07, paso 1d, regla 5). Son
  abstracciones genéricas, no conceptos de IA.
- ✅ *Aplicada* (2026-11-07, paso 1b). `02-architecture.md` §6: el esquema del store,
  escrito; este doc decidía **qué** guarda (ejecuciones, checkpoints), aquel dice cómo.

---

## 3. Módulos de serie

Imprescindibles para **usarlo**, no para que arranque (`11-composition.md` §3.1). La lista
vive en `[base] modules`. Cada caja es líquida: motor intercambiable, ranuras, se envuelve.
"De dónde sale" cita los sistemas que usan esa pieza (fuentes en §8).

### 3.1 Ya construidos

`agent`, `engine-claude`, `engine-opencode`, `providers`, `skills`, `communication`,
`tools`, `tools-mcp` (`12-agents.md`, `11-composition.md` §3). Cubren: motor con o sin el
harness del usuario (`profile = user | isolated`), ranuras de modelo por propósito,
plantillas de proveedor (incluidos modelos locales por HTTP compatible OpenAI: Ollama, LM
Studio, llama.cpp), skills, MCP, canal entre agentes.

### 3.2 Lógica

| Módulo | Nodos | De dónde sale |
|---|---|---|
| **`flow`** | ✅ **Llamar** (`call`, construido 2026-11-07: cada valor arranca una hija del workflow de su campo `workflow`; puertos declarados en el nodo con los ids y capacidades del workflow, más `done` (no se copian: `profile.check` avisa si no encajan); ejemplo `crates/modules/flow/examples/call.toml`) · **Condición/Switch** (reglas o enum) · **Bucle** hasta condición con tope N · **Unir** (fan-in; modo barrera: espera a los N) · **Fan-out** · **Esperar** · **Variable** · **Convertir tipo** (el módulo de compatibilidad entre dos cosas) | n8n IF/Switch/Merge/Loop, Node-RED switch/join, Dify Iteration/Aggregator, Langflow Type Convert; debate y Mixture-of-Agents piden la barrera |
| **`files`** | **Fichero → prompt** (la IA lee X) · **Prompt → fichero** (artefacto de fase) · **Documento fijo** (se inyecta cada turno: constitución, steering, rules) · **Observar fichero** | Ralph (`PROMPT.md`, `fix_plan.md`), Spec Kit (constitution, `specs/`), Kiro steering, ECC rules, BMAD story files, Manus `todo.md` |
| **`process`** | ✅ *Construido* (2026-11-07). **Comando** (`command`: un proceso por valor; `{{input}}` en `args`, `stdin` opcional, `cwd` por defecto el `node_dir`, `env` admite `secret:`; `stdout` si sale 0, `exit` siempre y `fail` si no, en `exit@1` `{code, signal?, ms, stdout, stderr, timed_out}`) · **Gate** (`gate`: corre el programa y reenvía **el mismo valor** por `pass` o `fail`; cualquier capacidad, `in`/`pass`/`fail` iguales o no arranca) · **Job largo** (`job`: log en `<node_dir>/logs/<n>.log`, `started` en `job@1`, `done` · `fail` · `timeout` con las últimas `tail_lines` líneas; `autostart`) · **Terminal/PTY** (`terminal`, `portable-pty`: arranca con la ejecución, `in` se teclea, `out` en trozos, `exit`). Cada proceso en su grupo y con hilos propios; `node/stop` manda SIGTERM, SIGKILL tras `grace_ms` y espera a que se recoja. Desvíos: `exit@1` y `job@1` viven en el módulo, no en proto; la cola del log viaja en los eventos (sin método `job.tail`); valores por defecto en campos del nodo (no hay `[process]`: los módulos no leen `config.toml`). Límites: un core que muere de golpe deja vivos sus procesos; lo que se escapa del grupo (`setsid`) no se sigue. Ejemplo `crates/modules/process/examples/job.toml` | Ralph backpressure (tests, tipos), PaperBench `reproduce.sh`, SWE-agent, pxl `build` |
| **`judge`** | **Juez LLM** (schema estricto, campo enum + `feedback`) · **Clasificador** · **Extractor** · **Rúbrica en árbol** (hojas pass/fail, padre media ponderada) | OpenAI structured outputs, CrewAI guardrails, Dify Question Classifier, PaperBench. **Motor intercambiable:** el de serie es "LLM con schema"; Alexander puede enchufar el suyo desde cero |
| **`guards`** (envolturas sobre cables "luego" y nodos) | ✅ *Construido* (2026-11-07). **Presupuesto** (`budget`: `max_items`, `max_ms` desde el primero, `max_cost` sumado de `cost_path`; pasado, el valor sale por `exceeded` y los siguientes también hasta `reset`; guarda checkpoint; componible en serie) · **Reintento** (`retry`: un valor en `fail` reenvía el último por `out` con backoff exponencial; tras `max_retries`, `gave_up`) · **Timeout** (`timeout`: reenvía y arma un reloj por valor; `ack` para el más antiguo; el que vence sale por `timeout`) · **Error** (rama de fallo: `error`, lo que llega a `fail` sale como `error@1` `{from, message, value, at}`) · **Watchdog** (`watchdog`: `in` lo pone `busy`, `progress` reinicia el reloj, `done` lo libera; `stall_ms` sin progreso → `stuck` y, con `restart = yes`, `restart`, como mucho `max_restarts`). Actúan **por paso del workflow**, por nodo y ejecución. Sin comodín en proto: el nodo declara `in`, `out` y los puertos que llevan el valor con la capacidad de su cable y `node/start` lo rechaza si no coinciden. Un hilo temporizador para todos; nada espera en el hilo del bus. Límite: `error` solo ve lo cableado a él; las entregas `failed` del bus y los arranques fallidos no llegan a ningún nodo (se ven en `regulus events` y en el estado). Ejemplo `crates/modules/guards/examples/backpressure.toml` (Ralph sin agente). Las envolturas **por petición al modelo** (Pool, Plan B, Router, Traductor) son otra cosa: van sobre `llm.endpoint` y necesitan la pasarela (`12-agents.md` §6.4) | AutoGen `MaxMessage \| TextMention`, LangGraph `recursion_limit`, Dify retry/fail branch, Make handlers, Gas Town Witness |
| **`triggers`** | **Cron** · **Webhook** · **Fichero cambia** · **Evento del bus** · **Chat** (entrada de un workflow chateable) · **Inactividad/umbral** | n8n Schedule/Webhook, Node-RED inject, Dify Start, Kiro `PostFileSave`, Letta sleep-time, Smallville reflexión por umbral |

### 3.3 Entorno

| Módulo | Nodos | De dónde sale |
|---|---|---|
| **`workspace`** | **Carpeta por ejecución** con cuota · motor **git-worktree** (crear, mergear, archivar, `.worktreeinclude`) · **Cola de merge** con gates · **Artefactos** | Conductor, Claude Squad, vibe-kanban, ccmanager, Orca, Gas Town Refinery; AI Scientist v1 casi llena 1 TB sin cuota |
| **`tasks`** | **Cola de tareas** con estados y dependencias · **Reclamar** con lock · **Ledger** (hechos, plan, progreso) | Gas Town beads, agent teams de Claude Code, vibe-kanban, Spec Kit tasks, Magentic-One ledgers |
| **`memory`** | **Store** leído y escrito por el agente (como herramienta y como inyección por hook) · **Reflexión** (buffer de fallos) · **Biblioteca de skills** que crece | Letta memory blocks, Reflexion, Voyager, ECC instincts |
| **`web`** | **HTTP** · **Fetch** · **Búsqueda** · **PDF** · **Descarga** de datasets | n8n/Dify HTTP, PaperBench `web_search` y lector paginado, Agent Lab (arXiv, HuggingFace) |
| **`human`** | **Aprobación** (pausa, formulario, timeout con rama, reanuda por id) · **Revisión** entre fases | Dify Human Input, LangGraph `interrupt`, A2A `input-required`, Spec Kit (una fase por comando revisado) |
| **`service`** (✅ decidido 2026-11-07) | Regulus **hacia fuera**: un workflow con puertos propios (§2 #3) se ofrece como **endpoint** a sistemas externos. **Llaves** generadas por Regulus, guardadas como secretos, con **ámbito** (qué proyectos y qué workflows puede llamar cada una), revocables una a una · **Política** por llave, por workflow o por llave×workflow, componible: **cooldown**, límite de peticiones por ventana, concurrencia, presupuesto (son envolturas de `guards` aplicadas en la puerta) · **Log** de peticiones al store (quién, qué, cuándo, cuánto tardó, resultado), consultable como `regulus events` · **Motor intercambiable**: HTTP de serie (síncrono y por job con `id` + sondeo); la misma caja puede servir WebSocket, MCP (`rmcp`, ya en `tools-mcp`) o compatible OpenAI sin tocar nada más · CLI equivalente (`regulus service key new|list|revoke`, `regulus service expose`). Hacer peticiones hacia fuera es `web`; recibirlas sin llave ni política es `triggers` Webhook. Al core no le pide nada nuevo: usa secretos, store (#2) y puertos del workflow (#3) | LiteLLM virtual keys (presupuesto y límite por llave y por equipo), Dify "API Access" (una llave por app), n8n webhook auth, Ollama/LM Studio como API local; **de memoria** |

---

## 4. La ranura `hooks` de la caja Agente

✅ **DECIDIDO (2026-11-07).** Los harnesses de Claude Code (ECC, Kiro, el bucle con
`Stop`) viven en **hooks**: eventos del ciclo de vida del agente (`PreToolUse`,
`PostToolUse`, `Stop`, `SessionStart`, `UserPromptSubmit`, `PreCompact`…) que pueden
**vetar** (exit 2) o **inyectar contexto** (`additionalContext`). La lista de métodos de
ACP no tiene nada equivalente.

Mismo truco que `tools-mcp` (`11-composition.md` §3.0): la caja Agente gana una entrada
`hooks` (varias) de capacidad `hook@1` (nombre provisional: evento que escucha, matcher,
y qué devuelve: veto, contexto o nada) y el motor la traduce. Claude: escribe los hooks en
el `settings.json` del **perfil aislado** apuntando a un puente `regulus hook` (oculto,
mismo binario, mismo `--home`) que llama al core y devuelve el veto o el contexto.
opencode: plugin. Con `profile = user` no se escribe nada en el perfil del usuario y queda
un aviso, igual que las skills (`12-agents.md` §3). Un motor que no lo soporte lo deja
fuera con aviso, como las ranuras.

**No es un cable "luego":** un hook es petición-respuesta con tiempo límite cuyo resultado
cambia el flujo del motor. Es un "usa" que el motor consume. Lo que un hook decide puede
ser una regla, un script (`process`) o un agente (`judge`): la caja conectada manda.

Cubre: ECC, Kiro (acción `agent`), Ralph con `Stop`, inyectar memoria o constitución cada
turno, restringir herramientas por modo (Cline Plan/Act, Roo Orchestrator).

✅ *Construido* (2026-11-07, paso 2; detalle en `12-agents.md` §3.1). Diferencias con lo
de arriba:

- **El core reenvía por capacidad, no conoce hooks.** Gana `hook.dispatch` (protocolo
  1.9) y el método reservado `node/hook`: pregunta a los nodos `hook@1` conectados al
  agente, cada uno en su hilo, y espera como mucho `[hooks] timeout_ms` (10 s). El que
  tarda, falla o no responde cuenta como "nada": el agente nunca se queda colgado. No
  sabe de eventos, matchers ni vetos; eso vive en los módulos.
- `hook@1` (`regulus-proto/src/hook.rs`): el nodo dice `event`, `matcher?`,
  `timeout_ms?` y `returns`; recibe `HookRequest` (`event`, `tool`, `input`, `text`… y
  todo lo del motor en `raw`) y responde `none`, `veto` + `reason` o `context` + `text`.
  Si hay varias respuestas, gana cualquier veto; si no, se suman los contextos.
- Eventos neutros: `pre_tool`, `post_tool`, `stop`, `session_start`, `prompt`,
  `pre_compact`. El matcher son nombres exactos con `|`, solo en eventos de herramienta.
- El puente lleva `--hook <nodo>`: una entrada por hook en la config del CLI, con su
  matcher y su tiempo, y `--returns`: una respuesta que el hook no declaró se descarta.
- **Falla abierto: si el hook no contesta, el agente sigue.** Tarde, con error, con una
  respuesta no declarada o con el core caído cuentan como "nada". Un hook puede pedir
  `fail = closed` (campo opcional de `hook@1`; el nodo `rule` lo tiene): entonces
  cualquiera de esos casos es un veto que dice por qué.
- Límites en `[hooks]`: `timeout_ms` (10 s), `max_timeout_ms` (600 s; el tiempo que pida
  una petición queda entre 1 ms y este) y `max_in_flight` (4 peticiones sin contestar por
  nodo hook; con más, se salta el nodo y se dice).
- opencode no lo soporta todavía (aviso al arrancar); ver `12-agents.md` §3.1.
- Primer módulo: `hooks`, nodo `rule` (veto o contexto fijos). Ejemplo en
  `crates/modules/hooks/examples/guarded.toml`.

---

## 5. Plantillas

Datos, no código (`11-composition.md` §7): un workflow guardado con huecos y `requires`.
Orden de construcción por tamaño; **cada una corre por CLI/TUI antes de la siguiente**.

| # | Plantilla | Piezas | Reconstruye |
|---|---|---|---|
| 1 | **pxl** | Agente + skill `pxl` ⇢ proveedor → Script `pxl build` (gate) → Fichero→prompt (`pxl ascii`) → Juez → bucle; Job largo `pxl serve` para el humano | [claude-pxl](https://github.com/crsolver/claude-pxl): la IA escribe `.px.js`, un CLI determinista genera PNG/GIF/JSON. Prueba que un dominio no-código se arma con las mismas piezas |
| 2 | **Ralph** | Bucle N + Fichero→prompt (`PROMPT.md`) + Agente con **contexto nuevo por vuelta** + Gate (tests) + Prompt→fichero (`fix_plan.md`) + Presupuesto | ghuntley.com/ralph. Variante con juez en vez de `<promise>COMPLETE</promise>` (frágil) |
| 3 | **SDD** | Cadena de fases (constitución → spec → plan → tareas → implementar) con Documento fijo, artefactos en disco, Revisión humana entre fases y Bucle `converge` | Spec Kit, Kiro, BMAD (personas = agentes con prompt + tools + modelo) |
| 4 | **Evaluador-optimizador** | Agente generador → Juez (enum + feedback) → bucle con tope | Anthropic "effective agents", CrewAI guardrails, Self-Refine |
| 5 | **Best-of-N** | Fan-out N Agentes en Workspace propio → Unir (barrera) → Juez → Cola de merge | Orca (`stablyai/orca`, probable), votación |
| 6 | **Equipo** | Cola de tareas + N Agentes con identidad (`participant.describe`) + Canal + Watchdog + Cola de merge con gates | Gas Town (mayor, polecats, witness, refinery), agent teams |
| 7 | **Research** | Orquestador → Fan-out subagentes (`Call`) → Web/PDF → Workspace con cuota → Job largo → Rúbrica → Presupuesto → Informe | AI Scientist v2 (etapas con reglas, juez solo al cierre), Agent Laboratory, PaperBench, deep research |
| 8 | **CLI custom chateable** | Trigger Chat → Agente (perfil aislado, Documento fijo como system prompt, ranuras de modelo) ⇢ MCPs; Supervisor (Clasificador) que decide qué `toolset` montar | la idea de Alexander: tu harness, hablado desde la app |
| — | **Router · Paralelo · Orquestador-workers · Cadena** | Clasificador + Switch; Fan-out + Unir; `Call` N; Agente → Gate → Agente | Anthropic "effective agents" |
| — | **Architect/editor** | Contador + Switch + dos Agentes con proveedores distintos, por turno | Aider. Alternar **por petición** dentro de un mismo Claude pide la pasarela (`12-agents.md` §6.4) |
| — | **Imagen al 50 %** | Chat → Agente ⇢ Pool ⇢ Gemini; Azar → Imagen blanca \| Generar → Script marca de agua → Salida | el ejemplo de Alexander |

---

## 6. Límites honestos

Lo que ningún diseño de nodos arregla, o que espera a otra pieza:

| Límite | Estado |
|---|---|
| **Que el modelo obedezca** un prompt (modos, personas, constitución, output styles) | No es problema de Regulus: los hooks fuerzan; los prompts no. Regulus es donde se inyecta |
| **Por petición** (alternar modelo, cascada barato→caro por calidad, contar coste, reescribir el prompt) | **Pasarela** (`12-agents.md` §6.4), aplazada hasta que una plantilla la pida. La envoltura que juzga la salida rompe el streaming |
| **Fork de sesión con retroceso** (Tree of Thoughts, LATS) | Depende del motor. `12-agents.md` §4 midió que opencode bifurca sesiones; Claude, por verificar. Puerta abierta: un `engine.session.fork` opcional en `agent.engine@1`, hoy inexistente |
| **CLI sin ACP** (detectar estado leyendo el PTY) | Aplazado, como el inyector de hcom |
| **Resultados fabricados** | Fallo nº1 en los sistemas de research revisados (MLR-Bench: los *coding agents* producen resultados falsos o inválidos en el 80 % de los casos). Por eso el **Gate determinista** es de primera clase y el juez LLM nunca es la única verificación |
| **Aislamiento de ejecución** (empresas, proyecto A no ve B) | ❓ fuera de v1 (`11-composition.md` §12). `project_id` ya existe en todo |
| **Enjambres y consenso** (claude-flow) | Sin evidencia de implementación; no se evalúa |

---

## 7. Orden de trabajo

Es la fila 4f de `plan.md` §3; las filas 5-6 (canvas editable, Operar) van después.
🔶 Las estimaciones son de Claude, sin medir: ~23 días de core y módulos más ~2 semanas
de app, unas 5 semanas en total. Se corrigen con el tiempo real al lado.

| # | Qué | Est. |
|---|---|---|
| 1 | ✅ Core: cable "luego" con dato + bus; store SQLite con ejecuciones persistentes; workflow con puertos propios; sub-ejecución `Call` (hecho 2026-11-07, 1 día) | ~5 días |
| 2 | ✅ Caja Agente: ranura `hooks` con puente `regulus hook` (hecho 2026-11-07, 1 día) | ~2 días |
| 3 | Módulos de lógica: `flow` · `files` · `process` · `judge` · `guards` · `triggers` | ~6 días |
| 4 | Módulos de entorno: `workspace` · `tasks` · `memory` · `web` · `human` · `service` | ~6 días |
| 5 | Plantillas de §5, en orden, cada una corriendo por CLI/TUI | ~4 días |
| 6 | Pasarela, solo cuando una plantilla la pida | — |
| 7 | App: canvas editable, Operar, instalar plantillas; la app misma editable y por configuración (`00-principles.md` §4) | ~2 sem |

Cada paso: `cargo test`, matriz de ablación, el ejemplo por CLI antes de tocar UI. Antes de
escribir una caja, buscar el crate que ya lo hace (cron, `rmcp`, `notify`…).

---

## 8. Fuentes

Cinco investigaciones del 2026-11-07 (herramientas visuales; patrones de agentes;
research; arquitecturas raras; harnesses de Claude Code). **Verificadas en fuente
primaria** salvo lo marcado.

- Herramientas visuales: [n8n](https://docs.n8n.io/flow-logic/), [Node-RED](https://nodered.org/docs/user-guide/nodes), [ComfyUI](https://docs.comfy.org/custom-nodes/backend/datatypes), [Dify](https://docs.dify.ai/en/use-dify/nodes/iteration), [Flowise](https://docs.flowiseai.com/using-flowise/agentflowv2), [Langflow](https://docs.langflow.org/concepts-components), [Make](https://help.make.com/error-handlers).
- Patrones: [Ralph](https://ghuntley.com/ralph/), [ralph-explained](https://github.com/rhinkle/ralph-explained), [Anthropic effective agents](https://www.anthropic.com/engineering/building-effective-agents), [LangGraph](https://docs.langchain.com/oss/python/langgraph/graph-api), [CrewAI](https://docs.crewai.com/en/concepts/tasks), [AutoGen termination](https://microsoft.github.io/autogen/stable/user-guide/agentchat-user-guide/tutorial/termination.html), [OpenAI handoffs](https://openai.github.io/openai-agents-python/handoffs/), [structured outputs](https://developers.openai.com/api/docs/guides/structured-outputs), [ACP](https://agentclientprotocol.com/protocol/overview), [A2A](https://a2a-protocol.org/latest/topics/life-of-a-task/), [hcom](https://github.com/aannoo/hcom).
- Research: [AI Scientist v1](https://arxiv.org/abs/2408.06292), [v2](https://arxiv.org/html/2504.08066), [Agent Laboratory](https://arxiv.org/html/2501.04227), [PaperBench](https://arxiv.org/html/2504.01848), [MLR-Bench](https://arxiv.org/abs/2505.19955), [SWE-agent](https://arxiv.org/html/2405.15793), [OpenHands](https://arxiv.org/html/2407.16741), [open deep research](https://www.langchain.com/blog/open-deep-research), [Ollama OpenAI compat](https://docs.ollama.com/api/openai-compatibility).
- Harnesses: [ECC](https://github.com/affaan-m/everything-claude-code), [Spec Kit](https://github.com/github/spec-kit), [Kiro hooks](https://kiro.dev/docs/hooks/), [Roo Boomerang](https://roocodeinc.github.io/Roo-Code/features/boomerang-tasks), [Cline Plan/Act](https://docs.cline.bot/features/plan-and-act), [Gas Town](https://github.com/steveyegge/gastown), [Conductor](https://www.conductor.build/docs/), [ccmanager](https://github.com/kbwo/ccmanager), [Claude Code hooks](https://code.claude.com/docs/en/hooks), [agent teams](https://code.claude.com/docs/en/agent-teams), [claude-pxl](https://github.com/crsolver/claude-pxl).
- **No verificadas en esta pasada** (de memoria, mecanismos conocidos, cifras por contrastar): Aider architect/editor, Letta/MemGPT, Voyager, MetaGPT/ChatDev/CAMEL, Generative Agents, Anthropic multi-agent research, Cognition, Manus, Devin, Magentic-One, Reflexion/ToT/LATS, debate/MoA, FrugalGPT/RouteLLM. También: Orca = `stablyai/orca` (por listados, no por su README), BMAD por dentro, claude-flow más allá del README.
