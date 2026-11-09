# 12 — Agentes: la caja y sus motores

> **v1.2** · 2026-10-17 · Respuesta de Alexander a la pregunta abierta de `plan.md` §2,
> más la investigación y la medición del mismo día, y los proveedores como caja líquida
> (`00-principles.md`). Manda sobre `old/07-ecosystem.md`.

---

## 1. La idea, en palabras de Alexander

> *"Un agente no es más que una caja que acepta varios tipos de agente: Claude,
> opencode… y la caja tiene su sistema de compatibilidad."*

✅ **DECIDIDO** — **Regulus no tiene agentes propios.** Tiene un nodo **Agente**, una caja
genérica, y dentro se elige el **motor**: Claude Code, opencode, Gemini CLI… Lo que se
conecta a la caja (proveedor, skills, MCP, comunicación) es igual sea cual sea el motor;
el motor traduce eso a su propio idioma.

```
 Proveedor: OpenRouter ⇢┐
 Skills: dev           ⇢┼─►  [ Agente ]  motor: Claude Code
 MCP: github           ⇢┘                instalación: ~/.local/bin/claude
                                         perfil: aislado | ~/.claude-ultra
```

## 2. Las piezas

| Pieza | Qué es | Dónde vive |
|---|---|---|
| **Caja Agente** | El nodo que ve el usuario. Entradas genéricas: un proveedor por propósito (§6.5), skills, MCP, mensajes | módulo `agent` |
| **Motor** | La compatibilidad con un CLI concreto | un módulo por motor: `engine-claude`, `engine-opencode`… |
| **Canal** | Cómo habla el motor con el proceso del CLI | dentro de cada motor (§4) |

✅ **DECIDIDO** — **Cada motor es un módulo aparte.** Ofrece la capacidad
`agent.engine@1`; la caja lista los motores instalados por esa capacidad, nunca por
nombre. Quitar `engine-claude` deja la caja funcionando con los demás (matriz de
ablación). Añadir un motor es añadir un módulo, sin tocar la caja.

## 3. Qué hace un motor

1. **Detecta** las instalaciones del PC: binario, versión. Para Claude, también los
   binarios de perfil (`claude-ultra-bin`, `claude-sub-bin`…). El usuario elige cuál
   usa cada caja.
2. **Declara** qué entradas de la caja soporta. Conectar algo que no soporta da un
   diagnóstico en `profile.check`, no un fallo en ejecución.
3. **Traduce** lo conectado a la configuración del CLI: variables de entorno, flags y
   ficheros de su perfil.
4. **Lanza y habla** con el proceso por su canal.

✅ **DECIDIDO** — **El perfil es seleccionable:** uno **aislado** que Regulus crea para esa
caja (solo tiene lo conectado), o **uno existente** del usuario (`~/.claude`,
`~/.claude-ultra`…), que hereda su setup entero. Para Claude, el aislado aplica el mismo
blindaje que `claude-sub` contra el `.claude/settings.json` del proyecto.

Los dos existen: el **existente** (`profile = user`, campo `config_dir`; vacío = el setup
por defecto del usuario) y el **aislado** (`profile = isolated`, §6.7), que necesita un
proveedor o una suscripción conectados para poder autenticarse.

| Entrada genérica | Motor Claude Code | Motor opencode |
|---|---|---|
| Instalación | `CLAUDE_CODE_EXECUTABLE` | binario elegido |
| Perfil | `CLAUDE_CONFIG_DIR` | su config |
| Proveedor (una ranura por propósito, §6.5) | `providers/set` de ACP: `anthropic`, `bedrock` o `vertex` + `baseUrl` | proveedor en su config |
| Skills | enlace en `CLAUDE_CONFIG_DIR/skills/<nombre>` | enlace en `OPENCODE_CONFIG_DIR/skills/<nombre>` |
| MCP | `mcpServers` al crear la sesión | igual |
| Hooks (§3.1) | `hooks` en `CLAUDE_CONFIG_DIR/settings.json` → `regulus hook` | no soportado: aviso |

**Skills, base construida (2026-10-17).** Una skill es un recurso: una carpeta con
`SKILL.md`, el formato que leen Claude Code y opencode. El módulo `skills` tiene un nodo
`skill` (campos `path` y `name`) que ofrece `skill@1`; la caja tiene la entrada `skills`
(varias) y pasa la lista al motor en `StartArgs.skills`. El motor dice dónde las lee su
CLI (`AcpEngine::skills_dir`) y `regulus-acp` las enlaza ahí, quitando antes los enlaces
de la vez anterior. Solo en perfil aislado: en el del usuario no se escribe nada y queda
un aviso. Probado: la skill aparece enlazada en el perfil de Claude, y con
`--without regulus/skills` la ejecución sigue funcionando.

**Un proveedor que no habla el protocolo del motor** (p. ej. solo OpenAI hacia Claude)
se resuelve con un **Traductor** en el cable "usa": Agente ⇢ Traductor ⇢ Proveedor. Es
una envoltura (§6.4) y la lectura A de las cajas (`11-composition.md` §9.1).

### 3.1 Ranura `hooks`

✅ **Construida (2026-11-07)** — `13-blocks.md` §4. La caja tiene la entrada `hooks`
(varias, `hook@1`). Al arrancar pregunta a cada hook qué escucha (método `hook`: `event`,
`matcher?`, `timeout_ms?`, `returns`) y pasa al motor `StartArgs.hooks`: por hook, el
comando del puente en nombre de la caja,
`regulus --home <H> hook --execution <id> --node <caja> --hook <nodo> --event <evento>
[--timeout-ms N]`. El host da el programa y su `--home`, como al puente MCP; un host sin
puente falla al arrancar una caja con hooks.

El motor los escribe como las skills: **solo en perfil aislado**, quitando lo de la vez
anterior; en el del usuario no escribe nada y avisa. Sin hooks conectados solo quita una
clave `hooks` vieja, y un `settings.json` que no entiende es un aviso, no un fallo.

**Si el hook no contesta, el agente sigue** (falla abierto): tarde, con error, con una
respuesta que no declaró en `returns` o sin core, es "nada". Con `fail = closed` en el
hook, cualquiera de esos casos es un veto.

**Acoplamiento conocido:** el puente (`regulus-cli`) enlaza `engine-claude` para hablar
el formato `claude`, igual que enlaza `tools-mcp` para MCP. Un registro de formatos llega
cuando un segundo motor tenga hooks. El puente lee el evento por stdin,
pregunta al core (`hook.dispatch`, que reenvía `node/hook` a los nodos conectados con su
tiempo límite) y responde como el CLI lee los hooks.

| Motor | Hooks |
|---|---|
| Claude Code | Regulus es dueño de la clave `hooks` del `settings.json` del perfil aislado (el que lee con `settingSources: ["user"]`). Eventos: `pre_tool` → `PreToolUse`, `post_tool` → `PostToolUse`, `stop` → `Stop`, `session_start` → `SessionStart`, `prompt` → `UserPromptSubmit`, `pre_compact` → `PreCompact`; uno desconocido queda fuera con aviso. Matcher solo en herramientas: los nombres se recortan y se unen con `\|` (`Bash \| Write` → `Bash\|Write`); un nombre que Claude leería como regex (con `.`, `*`…) se queda sin matcher, con aviso, y filtra el nodo. `timeout` = el del hook + 1 s. Puente: veto → exit 2 con el motivo en stderr; contexto → `hookSpecificOutput.additionalContext` (en `PreCompact`, stdout plano: instrucciones del resumen); nada → exit 0; core inalcanzable → exit 1 (error no bloqueante). Un veto en `post_tool` o `session_start` no para nada (Claude solo lo muestra): aviso al arrancar. Contexto en `stop` hace que Claude siga trabajando ("Stop hook feedback"): también avisa. `settings.json` se escribe entero y se renombra encima. Esquema: https://code.claude.com/docs/en/hooks |
| opencode | **No soportado**, aviso al arrancar. No tiene config de hooks, solo plugins en JS. Su API (`@opencode-ai/plugin` 1.17.18, leída en local) trae `tool.execute.before`/`after`, `chat.message`, `experimental.chat.system.transform` y `experimental.session.compacting`, pero nada que equivalga a `Stop` (seguir trabajando), el veto no está documentado (lanzar un error, por verificar) y el contexto va por hooks `experimental`. Hace falta un plugin puente, no un fichero. https://opencode.ai/docs/plugins no trae la referencia de hooks |

**Probado en vivo (2026-11-07)** con `claude` 2.1.319 de `~/.local/bin`, perfil aislado y
un endpoint Anthropic falso que pide `Bash` una vez: el `settings.json` del perfil salió
con `PreToolUse` (`matcher: Bash`) y `SessionStart` apuntando a `regulus hook`; la
llamada a `Bash` no se ejecutó y el modelo recibió como resultado *"PreToolUse:Bash hook
error: [...]: Bash is off for this agent: answer without running commands."*; y cada
petición con herramientas llevó *"SessionStart hook additional context: House rule: end
every answer with the word REGULUS."*. Al parar no quedó ningún proceso vivo.

## 4. El canal: ACP primero

**Qué es ACP** (Agent Client Protocol, de Zed): un idioma estándar para que una app
controle un agente de terminal. Abrir sesión, mandar un mensaje, recibir lo que hace
(herramientas, planes, permisos), reanudar o bifurcar sesiones. Lo hablan 43 agentes.
Sin él, Regulus tendría que aprender el método privado de cada CLI. hcom lo hizo así y
son ~549 KB de pegamento que se rompe con cada versión.

✅ **DECIDIDO** (Alexander, 2026-10-17) — **ACP es el canal por defecto; un motor puede
traer su canal nativo si ACP se le queda corto.** El canal vive dentro del motor, así que cambiarlo no toca
nada más. El código ACP es una librería compartida por los motores, no un módulo.

**Medido el 2026-10-17 en la máquina de Alexander:**

| Motor | Vía | Resultado |
|---|---|---|
| opencode 1.18.35 | `opencode acp`, nativo | handshake OK; sesiones: cargar, reanudar, bifurcar, listar |
| Claude Code 2.1.302 | adaptador `@agentclientprotocol/claude-agent-acp` 0.93.0 con `CLAUDE_CODE_EXECUTABLE` apuntando al binario real | sesión en 3,7 s; carga **su** setup: 102 comandos (plugins y skills), modos, modelo, esfuerzo |

**Costes y límites conocidos:**

- El adaptador de Claude es **Node**: el motor Claude depende de Node y de ese paquete.
  Alternativa si estorba: canal nativo `claude -p --input-format stream-json`, sin Node,
  pero con un protocolo de control sin spec pública.
- ACP v2 está en alfa y quitará `terminal/*` del cliente. No apoyarse en las RFD en
  borrador (proxy-chains, MCP-over-ACP).
- **Ningún canal se engancha a una sesión que un humano ya tiene abierta** en su
  terminal. Regulus lanza sus sesiones y puede reanudarlas desde disco. Ver el TUI real
  del agente es una vista opcional en PTY, no el canal de control.

## 5. Lo construido (2026-10-17)

| Pieza | Dónde |
|---|---|
| Contrato `agent.engine@1` | `crates/contracts/agent-engine`: lo comparten la caja y los motores, que no se conocen entre sí |
| Canal ACP + kit de motor | `crates/regulus-acp`: una sesión = un proceso; `EngineModule` sirve el contrato |
| Caja Agente | `crates/modules/agent`: entrada `engine` (obligatoria), campos `workdir` y `permissions` |
| Motor opencode | `crates/modules/engine-opencode`: `installation`, `config_dir` |
| Motor Claude Code | `crates/modules/engine-claude`: `installation`, `config_dir`, `adapter` |

**El contrato `agent.engine@1`:** `engine.detect` (instalaciones del PC),
`engine.session.start` (`cwd`, `permissions`), `engine.session.prompt` (`text` →
`stop_reason` + `text`), `engine.session.status` (`idle`, `busy`, `ended`, lo que ACP sabe del turno; opcional), `engine.session.cancel` (pide que acabe el turno en curso, `session/cancel` de ACP; el turno responde, normalmente con `cancelled`; opcional, 2026-11-08), `engine.session.events` y `engine.session.decide` (opcionales, 2026-11-09, más abajo) y `engine.session.stop`. `StartArgs.tools` lleva los montajes de herramientas (`11-composition.md` §3.0). Cambiar su forma es `agent.engine@2`. Los
contratos de capacidad viven en `crates/contracts/<capacidad>`, no dentro de ningún módulo.

**Cada motor anuncia además su propio método de detección** (`claude.detect`,
`opencode.detect`), para que la UI liste las instalaciones sin arrancar nada. Detectado en
la máquina de Alexander: `claude`, `claude-sub-bin` y `claude-ultra-bin` (cada uno con su
versión), y los dos `opencode`.

**Permisos:** cada caja elige `reject` (por defecto), `allow` o `ask` (2026-11-09: la
petición espera a quien decida, más abajo).

**Probado de punta a punta** con `regulus run agents` (`crates/modules/agent/examples/`):
las dos cajas arrancan, responden, conservan el contexto entre prompts y, al parar,
no queda ningún proceso vivo. Con `--without regulus/engine-claude`, la caja de Claude
queda desactivada y la de opencode sigue funcionando.

**La caja en cadenas "luego" (2026-11-08).** Entrada `prompt` (`text@1`, varias): cada
valor se manda a la sesión como `agent.prompt`; se encolan por nodo (campo `max_queue`,
16) y van de uno en uno en un hilo de la caja, sin bloquear el bus. Un valor que no cabe
en la cola va a `failed` con su texto (cortado a `failed_echo_chars`) y no se reintenta;
uno en curso cuando el nodo se para no da nada, ni `reply` ni `failed`. Salidas `reply`
(`text@1`, la respuesta final), `failed` (`text@1`, el error en su lugar, nunca las dos) y
`usage` (`json@1`, `{ input_tokens, output_tokens }` antes de `reply`, solo si el motor da
uso: `agent.engine@1` aún no lo lleva, así que hoy no sale). Campo `fresh_context`
(`no`/`yes`): con `yes` la caja cierra la sesión y abre otra antes de cada prompt tras el
primero, venga de `prompt`, de `agent.prompt` o de un canal. El motor opencode gana el
campo `model` (id de opencode, p. ej. `opencode/glm-5.3-flash-free`): orden **proveedor
conectado > campo `model` > el setup del propio opencode**. Claude Code no lo gana: su
modelo sale de su setup. Ejemplo `crates/modules/agent/examples/then.toml`, probado por
CLI con el modelo gratis: dos prompts dan dos `reply` y, con `yes`, dos sesiones de
opencode; con `no`, una.

✅ **Entrada `workdir` (2026-11-08).** `path@1`, "luego", una: mueve al agente. Los
turnos que empiezan después de que llegue un valor corren en ese directorio, y los
siguientes también hasta que llegue otro; el campo `workdir` es dónde corre mientras no
llega nada, y un texto vacío vuelve a él. Acepta la ruta como texto o un objeto con `path`
(el `info` de un workspace); lo normal es cablear `workspace.ready`. **Cambiar de
directorio cierra la sesión y abre otra, y el contexto se pierde:** ACP fija el cwd en
`session/new` (`regulus-acp` lo manda ahí; el proceso del CLI sigue con el cwd de
`serve`), así que el valor del turno gana abriendo una sesión nueva en él. Un valor que no
es un directorio absoluto existente se rechaza al llegar y se guarda igual: los turnos
siguientes fallan por `failed` en vez de correr en otro sitio. El cwd es dónde empieza el
agente, no una valla: con `allow` puede escribir fuera por ruta absoluta. Lo usan
`best-of-n`, `team` y `research` (`13-blocks.md` §5).

✅ **Turno con tope (2026-11-08).** Campo `turn_timeout_ms` (0 = sin límite): un turno que
no da ni `reply` ni `failed` en ese tiempo (proveedor sin cuota que reintenta por dentro,
adaptador colgado) se cancela con `engine.session.cancel` y sale por `failed` como
"turn timed out after N ms (it was cancelled)". Si el motor no puede cancelar, el turno
acaba con error tras la cancelación o sigue vivo pasado `cancel_grace_ms` (5000), la caja
cierra la sesión ("turn timed out after N ms; <qué pasó>: its session was dropped") y el
turno siguiente abre otra, sin contexto. Una respuesta que llega justo al vencer es la del
turno: sale por `reply`. Al comparar directorios para decidir si se mueve, la caja resuelve
`..` y enlaces: `/a/../b` y `/b` son el mismo y no abren sesión. Vale para los tres caminos de
prompt (`prompt`, `agent.prompt`, canal). `regulus-acp` lo sirve: el turno escucha la
cancelación mientras lee y manda `session/cancel` al agente. Probado contra un motor
falso y contra el agente ACP de prueba (`hang` responde a la cancelación, `deaf` no).

✅ **Eventos, historial, permisos `ask` y reanudar (2026-11-09).** Antes la caja solo veía
el texto final de cada turno.

- **Eventos de sesión.** `agent.engine@1` gana, sin romper `@1`, `engine.session.events`
  `{session_id, since}` → `{events, next}` (`since` 0: desde el más viejo guardado; `next`
  es el `seq` del último dado). Cada evento es `{seq, at, turn?, kind, …}`: `turn_started`,
  `text`, `thought`, `tool_call` (`id`, `title`, `tool_kind?`, `status`; `tool_kind` y no
  `kind` porque `kind` es la etiqueta), `tool_update` (`id`, `status`, `content?`), `plan`
  (`entries`), `permission_request` (`id`, `tool?`, `title`, `options: [{id, kind, name}]`),
  `permission_resolved` (`id`, `option`; `null` = cancelada), `turn_ended` (`stop_reason`)
  y `state` (`idle`/`busy`/`ended`). `regulus-acp` guarda por sesión los últimos
  `events_keep` (campo de los motores, 2000) a partir de las updates de ACP que sabe leer
  (chunks de texto y de razonamiento, `tool_call`, `tool_call_update`, `plan`); el resto
  se ignora. `Started` gana `capabilities: {load_session, events}`, `native_session_id`
  (el id de la sesión en el CLI) y `resumed`; `Turn` gana `usage?`, que `regulus-acp` no
  rellena: el uso por turno de ACP v1 es inestable y el crate no lo da.
- **Permisos `ask`.** Con `ask` cada petición del agente sale como `permission_request` y
  espera a `engine.session.decide` `{session_id, request_id, option}` (`option` `null`:
  cancelarla). Un `cancel` del turno la cancela, como pide ACP; pasado
  `permission_timeout_seconds` (campo de los motores, 600) se elige la opción `reject…`, o
  se cancela si no la hay. `allow` y `reject` siguen contestando solos. La caja tiene la
  entrada "usa" `decisions` (`decision@1`, opcional): por cada petición llama a
  `decision.ask` (prompt "Agent `<nombre>` asks permission: <título>", las opciones con su
  `kind` y el contexto `{tool, title, execution_id, node_id, request_id}`), sondea
  `decision.poll` cada `decision_poll_ms` (500) y contesta al motor con lo elegido; sin
  opción (rechazo sin opción de rechazo, plazo sin `default_option`) o si el decisor
  falla, la opción `reject…`. Lo de serie es el **Decisor** de `human`
  (`13-blocks.md` §3.3): la petición sale en `regulus human list` y se contesta con
  `regulus human answer <id> approve --field option=<id>`. Con `ask` y nada en
  `decisions`, la caja arranca con `reject`, lo dice en el log y deja una entrada
  `system` en el historial.
- **Historial.** Con `history = yes` (por defecto) la caja guarda la conversación en su
  store, en la base del proyecto del nodo, bajo `conv/<workflow>/<nodo>`: sobrevive a la
  ejecución y al reinicio, y la siguiente ejecución del mismo nodo sigue en ella. Entradas
  `{seq, at, role: user|agent|system, text, turn, execution_id}` (`turn` = `t-<n>`) y
  actividad `{seq, at, turn, event}`; las dos comparten un solo `seq`, así que un cursor
  las pagina juntas sin perder ni repetir nada. Se queda con las últimas `history_max`
  (2000) de cada lista. Durante el turno la caja lee los eventos cada `events_poll_ms`
  (200) en el hilo del turno, y una vez más al acabar. Sin store vive en memoria; sin
  directorio de datos la clave va por ejecución.
- **Salida `activity`** (`agent.event@1`, "luego"): cada evento leído, su JSON tal cual, si
  `activity_emit = yes` (por defecto). Un motor sin eventos (`UnknownMethod`) deja la caja
  con solo el texto final, como antes.
- **Métodos nuevos** (además de `agent.prompt`, que sigue bloqueando): `agent.send`
  `{execution_id, node_id, text}` → `{turn}` (a la misma cola que la entrada `prompt`, sin
  esperar), `agent.history` `{execution_id, node_id, since?, limit?}` → `{entries,
  activity, next, session: {engine_session_id, native_resume, state}}` (lee antes los
  eventos pendientes; de un nodo parado responde con `state: "stopped"`) y `agent.cancel`
  `{execution_id, node_id}` → `null` (`engine.session.cancel`). CLI: `regulus agent send`,
  `regulus agent history [--since N] [--follow]` y `regulus agent cancel`.
- **Reanudar.** `resume = native` (por defecto `no`; nunca con `fresh_context = yes`): si
  el motor dice `load_session`, la caja guarda el `native_session_id` en la conversación y
  la siguiente ejecución lo pasa en `StartArgs.resume`; `regulus-acp` lo carga con
  `session/load` y descarta lo que el agente reproduce al cargar (es historia, no
  novedad: no entra en los eventos). Si el motor no puede o la carga falla, abre una
  sesión nueva, lo avisa, y el historial dice que no se reanudó. **Reproducir el
  historial nunca cuenta como reanudar**: `native_resume` solo es `true` si el CLI cargó
  su sesión. Comprobado en local el `initialize` (2026-11-09): anuncian `loadSession: true`
  opencode 1.19.5 (`opencode acp`) y `@agentclientprotocol/claude-agent-acp` 0.93.0; la
  carga real con un CLI de verdad no está verificada, solo contra el agente de prueba.

Probado con un motor falso en proceso (`crates/modules/agent/tests/events.rs`: historial
y `activity`, paginado en dos páginas, la petición contestada con `human answer`, `ask`
sin decisor, `agent.cancel`, reanudar con y sin `load_session`) y por ACP con el agente de
prueba (`crates/regulus-acp/tests/events.rs` y `agent_events.rs`: eventos con cursor y
búfer acotado, `ask` → `decide`, plazo, cancelar, `session/load` sin repetir lo cargado; y
la caja con `human` de punta a punta). Límites: el orden entre una `permission_request` y
las updates que la preceden en el mismo instante no está garantizado; un hilo por petición
pendiente sondea al decisor hasta que decide, el motor la resuelve o el nodo para.

## 6. Proveedores

✅ **DECIDIDO** (Alexander, 2026-10-17: *"lo más flexible"*). Ese mismo día se amplió: el
proveedor es una **caja líquida** (`00-principles.md`), con motor, envolturas y
plantillas, y la caja Agente gana una ranura de proveedor por propósito.

```
 [Agente]
   principal ◄──── [Plan B] ◄── 1. [Pool claves] ◄── opencode Go ×2
                              └─ 2. OpenRouter
   pensar    ◄──── Anthropic (Opus, esfuerzo alto)
   rápido    ◄──── [Router] "lo barato que haya" ◄── z.ai · local
   subagente ◄──── Ollama local
   imágenes  ◄──── [Generador] → se le da al agente como herramienta (MCP)
```

### 6.1 La capacidad `llm.endpoint@1`

Un proveedor **describe** dónde está el modelo y el CLI habla con él directamente.
Mientras nadie pida tocar cada petición, Regulus no está en el tráfico: no añade latencia
ni rompe el streaming del CLI. Cuando hace falta tocarlo, entra la pasarela (§6.4).

```
 Proveedor ⇢ llm.endpoint ⇢ [ Agente ] ── engine.session.start + provider ──► Motor
```

El nodo Proveedor responde a `llm.endpoint` con:

```json
{ "protocol": "anthropic", "base_url": "https://openrouter.ai/api",
  "headers": { "authorization": "Bearer …" }, "models": ["…"] }
```

- `protocol` puede ser `anthropic`, `openai`, `bedrock` o `vertex`. Es un dato, no un
  tipo cerrado: un protocolo nuevo no cambia la capacidad.
- `models` es opcional. El primero es el modelo por defecto.
- La caja resuelve su entrada de proveedor y pasa la descripción al motor en
  `StartArgs.provider`. Es un campo nuevo opcional, así que sigue siendo
  `agent.engine@1` (`03-modules.md` §1).
- **Cada motor declara los protocolos que habla** (Claude: `anthropic`, `bedrock`,
  `vertex`; opencode: `anthropic`, `openai`). Si no coincide, el arranque falla con un
  error que dice qué falta. La solución es el Traductor (§6.4).
- 🔶 **Protocolo por modelo.** Un mismo host puede servir unos modelos por Anthropic y
  otros por OpenAI (opencode Go: qwen y minimax por `/messages`; glm, kimi y deepseek
  solo por `/chat/completions`). Llega como campo opcional, así que sigue siendo `@1`. Su
  forma la fija el primer módulo que lo use (`11-composition.md` §10).
- `llm.chat@1` queda reservado para cuando sea Regulus quien llame a un modelo.

**Cómo lo traduce cada motor de agente:**

| Motor | Vía |
|---|---|
| Claude Code | `providers/set` de ACP (`apiType`, `baseUrl`, `headers`), antes de abrir la sesión |
| opencode | `OPENCODE_CONFIG_CONTENT` con un proveedor `regulus` (`@ai-sdk/anthropic` o `@ai-sdk/openai-compatible`) y `model = "regulus/<modelo>"` |

### 6.2 La caja Proveedor y sus motores

Como la caja Agente: **una caja genérica, y dentro el motor que la hace funcionar.** Cada
motor es un módulo; la caja los encuentra por capacidad, nunca por nombre.

| Motor | Qué hay detrás | Qué se configura |
|---|---|---|
| **API** | un servicio remoto: OpenRouter, opencode Go, Anthropic, z.ai | plantilla, credencial, cabeceras extra, modelos |
| **Local** | un servidor en el PC: Ollama, LM Studio, llama.cpp, ya arrancado o lanzado por Regulus | arrancarlo y pararlo con la ejecución, pedirle sus modelos, comprobar que responde |
| **Suscripción** | el login de un plan (§6.6) | login, renovar el token, qué motores de agente lo pueden usar |

- **La caja** guarda lo que comparten todos los motores: modelos ofrecidos, cabeceras
  extra y protocolo por modelo. Responde `llm.endpoint`.
- **El motor** sabe cómo llegar: URL, autenticación y, si es local, el proceso.
- 🔶 El nombre de la capacidad del motor (`llm.source@1`) y el reparto exacto de campos
  entre caja y motor se fijan al construirlo. Hoy el módulo `providers` es un único nodo
  que hace de caja y de motor API (§6.8).

### 6.3 Plantillas

OpenRouter, opencode Go, Anthropic, z.ai o un proxy local **no son código: son
plantillas** del motor API. Viven en un catálogo, un fichero de datos dentro del módulo.
El nodo elige `preset = "openrouter"` y se rellenan solos la URL, el protocolo, la
autenticación, las cabeceras y los modelos. Cualquier campo se puede pisar a mano.
Añadir un proveedor es añadir unas líneas al catálogo, sin recompilar.

**Cabeceras extra:** cualquier cabecera que el proveedor pida o permita, como
`x-opencode-session` (opencode Go), o `HTTP-Referer` y `X-Title` (OpenRouter). Un valor
puede ser `secret:<nombre>`.

**Cada plantilla lleva sus condiciones de uso** en una línea, igual que el catálogo de
caveman en `alx-claude-setup`. Por ejemplo: opencode Go permite su clave en otros
agentes, siempre que cada uno mande su propio User-Agent y un `x-opencode-session`
estable. Las condiciones deciden qué puede hacer el Pool (§6.4) y qué motores pueden usar
una suscripción (§6.6).

### 6.4 Envolturas y la pasarela

Una **envoltura** es una caja que entra y sale por `llm.endpoint`, así que se mete en
cualquier cable de proveedor y se encadena sin límite:

| Envoltura | Qué hace |
|---|---|
| **Pool** | Varias claves o cuentas que se reparten y rotan al llegar al límite, si las condiciones del proveedor lo permiten |
| **Plan B** | Prueba en orden; si el primero falla, pasa al siguiente |
| **Router** | Decide por reglas: modelo, coste, tamaño de la petición, hora |
| **Traductor** | De OpenAI a Anthropic y al revés |

Todo lo que actúa **en cada petición** (reintentar, rotar, cambiar de proveedor,
traducir, contar tokens) necesita algo en medio del tráfico. Ese algo es la **pasarela**,
un proceso de Regulus.

✅ **Regulus compila el dibujo; el usuario no elige "directo o pasarela".**

- Si la cadena de una ranura acaba en un único proveedor sin envolturas, el CLI habla
  **directo** con él. Cero latencia, como hasta ahora.
- Si hay envolturas, o varias ranuras con proveedores distintos en un CLI que solo acepta
  una URL (Claude Code), Regulus arranca **una pasarela** que ejecuta ese dibujo y le pasa
  al CLI la URL de la pasarela. Deja pasar el streaming sin tocarlo.
- La pasarela ve todo el tráfico, así que **cuenta tokens, coste y errores** sin más
  trabajo y lo publica en el bus ("opencode Go al 80 %", "OpenRouter cayó, usando plan
  B"). Operar lo muestra y otros nodos pueden reaccionar.

La pasarela es una pieza **genérica del producto**. No es token-proxy ni headroom, que
siguen siendo de la cadena personal de Alexander (`plan.md` §4.3).

### 6.5 Las ranuras de la caja Agente

La caja Agente no tiene una sola entrada de proveedor, sino **una por propósito**. Cada
motor de agente declara cuáles entiende y las traduce a los ajustes de su CLI. Conectar
una ranura que el motor no entiende da un aviso en `profile.check`.

| Ranura | Claude Code | opencode |
|---|---|---|
| **principal** | `ANTHROPIC_DEFAULT_OPUS_MODEL` / `_SONNET_MODEL` | `model` |
| **rápido** (tareas de fondo, títulos) | `ANTHROPIC_DEFAULT_HAIKU_MODEL` | `small_model` |
| **subagentes** | `CLAUDE_CODE_SUBAGENT_MODEL` | el modelo de cada agente de su config |
| **pensar** | por verificar (`opusplan`, esfuerzo); aún no es una ranura | el modelo del agente `plan` |

Variables comprobadas en los binarios instalados el 2026-10-17 (Claude Code 2.1.302,
opencode 1.18.35).

- La ranura **principal** es la entrada `provider` que ya existe. Las demás son
  opcionales; si no se conectan, usan la principal.
- Cada ranura puede llevar también **modelo y esfuerzo**. Son opciones de uso y van en la
  caja, no en el proveedor: un mismo OpenRouter alimenta a una caja con Opus a esfuerzo
  alto y a otra con GLM barato.
- **Imágenes** no es un modelo del CLI: ni Claude Code ni opencode generan imágenes. Un
  proveedor de imágenes se le conecta al agente **como herramienta**, por MCP.

**Construido y medido (2026-10-17): las ranuras `fast` y `subagent`.** La caja trata
como ranura cualquier entrada `llm.endpoint@1` de su manifest que no sea `provider`, así
que añadir una ranura es tocar solo el manifest. Llegan al motor en
`StartArgs.slots`, un campo opcional que mantiene `agent.engine@1`, y cada motor declara
las suyas. Una ranura que el motor no conoce se deja fuera con un aviso: va en
`Started.warnings` y en el log del servidor, porque el core todavía no guarda lo que
responde el arranque de un nodo. La pregunta en `profile.check` llega cuando el core
sepa preguntar a los motores.

- **Claude Code:** las ranuras van como variables de entorno de la sesión
  (`_meta.claudeCode.options.env`), que el adaptador mezcla en el entorno de `claude`.
  En directo solo pueden elegir otro modelo **del mismo endpoint** que la principal; si
  apuntan a otro sitio, el arranque falla diciendo que hace falta la pasarela.
- **opencode:** admite varios proveedores a la vez, así que cada ranura puede ir a su
  propio endpoint sin pasarela: `small_model` para `fast` y el modelo de sus subagentes
  `general` y `explore` para `subagent`.

Probado contra el endpoint de prueba, con las dos cajas en perfil aislado: opencode pidió
`fast-model` para su tarea de fondo y `main-model` para responder, y el proceso `claude`
arrancó con `ANTHROPIC_DEFAULT_HAIKU_MODEL=fast-model`. En una sesión tan corta, Claude
no lanzó ninguna tarea de fondo, así que no llegó a pedir ese modelo. Al parar no quedó
ningún proceso vivo.

### 6.6 Suscripciones

- **Claude Pro/Max en el motor Claude Code:** con perfil `user` ya funciona, porque hereda
  el login. Para el perfil aislado, el motor Suscripción guarda como secreto el token de
  `claude setup-token`, y el motor Claude lo pasa en `CLAUDE_CODE_OAUTH_TOKEN` en lugar de
  usar `providers/set`. Su protocolo es `anthropic-oauth`, y **solo el motor Claude lo
  declara**.
- **Claude Pro/Max en opencode u otro CLI: no.** Anthropic solo permite ese login en sus
  propias apps. Como opencode no declara `anthropic-oauth`, el arranque falla con un
  error claro: la regla se cumple sola, sin un caso especial en el código.
- **opencode Go en Claude Code:** sí, por el motor API con su plantilla (§6.3). Para los
  modelos que solo hablan OpenAI hace falta el Traductor.

**Construido y medido (2026-10-17): Claude Pro/Max con `anthropic-oauth`.** Todavía sin
motor Suscripción: es la plantilla `claude-subscription` del nodo Proveedor, con
`credential = "secret:claude-sub"` y el token de `claude setup-token` guardado con
`regulus secret set claude-sub`. El proveedor lo manda como `authorization: Bearer`, como
cualquier credencial, y el motor Claude, al ver `anthropic-oauth`, no usa
`providers/set`. Lo pasa en el entorno de la sesión (`_meta.claudeCode.options.env`):

- `CLAUDE_CODE_OAUTH_TOKEN` con el token, y `ANTHROPIC_BASE_URL` con la URL del
  proveedor, para que una del entorno del usuario no se lleve el token a otro sitio.
- `ANTHROPIC_API_KEY` y `ANTHROPIC_AUTH_TOKEN` vacías: si el entorno del usuario trae
  una, Claude Code la usaría antes que el login.
- Las cabeceras extra del proveedor van en `ANTHROPIC_CUSTOM_HEADERS`.

Probado contra el endpoint de prueba con perfil aislado: la petición llegó con
`Authorization: Bearer <token>`, sin `x-api-key` y con el modelo del proveedor, y la
caja respondió. La misma plantilla en una caja con opencode falla al arrancar: *"opencode
engine speaks anthropic, openai; the `provider` provider speaks `anthropic-oauth`"*. Sin
un token real no se ha probado contra Anthropic; renovar el token sigue siendo cosa del
motor Suscripción.

### 6.7 El perfil aislado

El motor de agente elige perfil con el campo `profile`:

- `user` (por defecto): el comportamiento actual. Usa `config_dir`, y si está vacío, el
  setup del usuario.
- `isolated`: un perfil vacío que solo tiene lo conectado. Vive en
  `ModuleCtx::node_dir` (`03-modules.md` §2.1), o en `config_dir` si se rellena. Se
  conserva entre ejecuciones para poder reanudar sesiones.

Nada se escribe en el perfil del usuario. Cómo aísla cada motor:

| Motor | Aislado |
|---|---|
| Claude Code | `CLAUDE_CONFIG_DIR` al perfil y `settingSources: ["user"]` en la sesión: no lee el `.claude/` del proyecto (el blindaje de `claude-sub`) |
| opencode | `XDG_CONFIG_HOME`, `XDG_DATA_HOME` y `XDG_STATE_HOME` dentro del perfil, y `OPENCODE_DISABLE_PROJECT_CONFIG`, `_CLAUDE_CODE` y `_EXTERNAL_SKILLS`. La caché sigue compartida: no guarda nada del usuario |

### 6.8 Lo construido y lo medido (2026-10-17)

Construida la primera versión: un único nodo Proveedor genérico, con los campos
`protocol`, `base_url`, `credential` (`secret:<nombre>`), `auth` (`bearer`, `x-api-key`
o `none`) y `model`. Hace de caja y de motor API a la vez, todavía sin envolturas ni
ranuras.

**Plantillas y cabeceras extra (§6.3), construidas:** campo `preset`, catálogo de serie
en `crates/modules/providers/presets.toml` (openrouter, opencode-go, anthropic,
claude-subscription, zai-coding-plan, deepseek, moonshot, ollama…), un catálogo propio opcional con el campo
`catalog`, y el campo `headers`. Cada ajuste sale del nodo, si no de la plantilla, y si
no del valor por defecto del manifest. Un host con dos protocolos tiene una plantilla por
protocolo (`opencode-go` y `opencode-go-openai`). Así, el protocolo por modelo (§6.1)
sigue pendiente hasta que una sesión necesite mezclar los dos.

| Pieza | Dónde |
|---|---|
| Contrato `llm.endpoint@1` | `crates/contracts/llm-endpoint` |
| Módulo Proveedores | `crates/modules/providers`; ejemplo en `examples/local.toml` |
| Entrada `provider` de la caja | `crates/modules/agent` |
| Protocolos, perfil y peticiones previas a la sesión | `crates/regulus-acp` (`AcpEngine::prepare`) |
| Secretos y `node_dir` | `crates/regulus-core` (`secrets.rs`, `ModuleCtx`); CLI `regulus secret` |

**Probado de punta a punta** contra un endpoint Anthropic de prueba: las dos cajas, en
perfil aislado y con el mismo proveedor, responden. El endpoint recibió la clave sacada
del almacén de secretos y el modelo del proveedor. Claude entró por `providers/set` y
opencode por su config. Sin el secreto, la ejecución falla diciendo cómo crearlo. Con
`--without regulus/providers`, las cajas siguen funcionando. Al parar no queda ningún
proceso vivo.

**Si un agente muere,** el error del prompt trae lo último que escribió por stderr, en
lugar de un "session is closed" que no dice nada.

**Límite conocido:** `vertex` necesita proyecto y región, que `llm.endpoint@1` todavía no
lleva. Llegarán como campo opcional cuando alguien conecte Vertex.

## 7. Orden resultante

Sustituye a las dos ramas de `plan.md` §2:

1. ✅ Caja **Agente** + **motor opencode** + **motor Claude** (canal ACP)
2. ✅ **Proveedores**, primera versión (§6.8)
3. **Proveedores como caja líquida**, cada paso cuando algo lo pide:
   1. ✅ Plantillas y cabeceras extra (§6.3), y las ranuras `fast` y `subagent`, en
      directo (§6.5). El protocolo por modelo espera a que alguien lo necesite
   2. ✅ Suscripción de Claude con `anthropic-oauth`, como plantilla (§6.6)
   3. ⏸ Pasarela con Plan B y Pool (§6.4): aplazada, la estructura basta por ahora
   4. ⏸ Router, Traductor, motor Local e imágenes por MCP
4. ✅ **Skills**, base (§3), y ✅ **hooks** (§3.1)
5. **Comunicación** (hcom, sobre sesiones ACP: `old/07-ecosystem.md` §1.4)

Precedente más cercano: vibe-kanban (Rust) mezcla canales por agente (stream-json,
`app-server`, `serve`, ACP) y anuncia su cierre. No absorber sin la matriz de ablación.

## 8. El motor nativo

✅ *Construido* (2026-11-09, contrato C10). Un motor más de la caja Agente, pero sin CLI
detrás: el bucle lo corre Regulus. Módulo `regulus/engine-native` (`crates/modules/
engine-native`), nodo `native`, ofrece `agent.engine@1` entero: `start`, `prompt`,
`status`, `cancel`, `stop`, `events` y `decide`, con `capabilities = {events: true,
load_session: false}`. La caja, su historial, `activity` y el Decisor funcionan igual
que con Claude Code u opencode.

```
 [Agente] ⇢ engine ⇢ [native] ⇢ model   ⇢ Proveedor (openai | anthropic)
                               ⇢ tools   ⇢ memory.store · documents.retrieve · …  (tool@1)
                               ⇢ context ⇢ files.document · memory.inject · rule …  (hook@1)
 [Agente] ⇢ tools ⇢ [tools] ⇢ engine ⇢ [tools-mcp]   (toolset@1: los montajes de la caja,
                                                      los mismos que reciben Claude y opencode)
```

**Entradas.** `model` (`llm.endpoint@1`, obligatoria: sin ella el nodo queda
desactivado), `tools` (`tool@1`, varias: se llaman **directamente** con `tool.call`, sin
MCP ni puente; se vuelven a listar al empezar cada turno, y un nombre repetido entre dos
nodos se queda con el primero y lo dice en el log) y `context` (`hook@1`, varias). Además,
los **montajes de la caja** (`StartArgs.tools`, ver abajo).

**Montajes de la caja** (2026-11-09). Lo que la caja Agente tiene cableado en su `tools`
(`toolset@1`: caja `tools` + `tools-mcp`) llega al motor como `Mount`s, igual que a Claude
Code u opencode; cambiar de motor no pierde herramientas. El motor arranca cada montaje
`mcp-stdio` como proceso propio (su `command`, `args` y `env` tal cual: el `env` lleva el
binding con el nombre de quien llama, así que las llamadas van en nombre de la caja), habla
el MCP justo para herramientas (`initialize`, `tools/list` con páginas, `tools/call`,
`notifications/cancelled`; el cliente está en `engine-native/src/mcp.rs`, sin dependencias
nuevas) y vuelve a listar al empezar cada turno. Sus herramientas pasan por **lo mismo** que
las cableadas: política, `allow_tools`/`deny_tools`, permisos, hooks `pre_tool`/`post_tool`,
eventos y `max_tool_calls`. **Nombres:** primero las de `tools`, en orden de cables; luego
las de cada montaje, en el orden de la caja. Una de montaje con nombre ya usado va como
`<montaje>__<nombre>` (con aviso); si también está usado, se deja fuera con aviso. Un
montaje de otro protocolo, uno que no arranca o no contesta a `initialize` en
`mount_start_timeout_ms`, y un `tools/list` que falla al arrancar salen en
`Started.warnings` (durante la sesión, en el log); los valores del `env` de 8 caracteres o
más se ocultan (`[hidden]`) en esos mensajes y en los errores que ve el modelo. Una llamada
espera como mucho `mount_call_timeout_ms` (pasado, se cancela en el servidor y el modelo
recibe un error); `cancel` corta la espera al momento y avisa al servidor; `stop`, `node/stop`
o un arranque fallido matan los procesos. La espera no bloquea el bus: cada proceso tiene su
hilo lector y el turno espera en su propio canal.

**El bucle.** Mensajes = sistema (campo `system` + contexto de `session_start` + contexto
de `prompt` de este turno) + los últimos `history_turns` turnos (con sus llamadas y
resultados) + el prompt. Se pide al modelo; por cada llamada a herramienta: la política
decide, se llama, el resultado vuelve como observación (cortado a
`observation_max_chars`); y otra vez, hasta que el modelo responde sin herramientas o se
gastan `max_steps` llamadas al modelo (`stop_reason = max_steps`). Pasadas
`max_tool_calls` llamadas en el turno, cada llamada más recibe un error. Formatos: OpenAI
Chat Completions (`tools`/`tool_calls`, `role: tool`) y Anthropic Messages (`tool_use`/
`tool_result`, con `anthropic-version` del protocolo); `llm.endpoint@1` dice cuál con
`protocol`. `anthropic-oauth` no se acepta: la suscripción de Claude es solo para sus
apps (§6.6). El uso de tokens se suma por turno y sale en `Turn.usage`, así que la salida
`usage` de la caja sí sale con este motor cuando el proveedor da uso.

**Campos** (todos con valor por defecto): `model` (vacío: el primero del proveedor),
`system`, `name` (quién llama a las herramientas; vacío: el id del nodo), `max_steps`
(16), `max_tool_calls` (32), `tool_policy` (`allow` | `ask` | `deny`, `ask`),
`allow_tools` y `deny_tools` (listas; `deny` gana), `pre_tool_context` (`note` | `allow`
| `ask`, `note`), `stop_when` (`no_tool_call` | `text`), `temperature` (por debajo de 0:
no se manda), `max_tokens` (4096), `history_turns` (20; 0: ninguno),
`observation_max_chars` (20000; 0: sin corte), `request_timeout_ms` (120000),
`hook_timeout_ms` (10000), `events_keep` (2000), `permission_timeout_seconds` (600),
`mount_start_timeout_ms` (30000) y `mount_call_timeout_ms` (300000).

**Lo cableado manda; los campos son lo que pasa si nada dice otra cosa.** Los hooks de
`context` se preguntan como pregunta el puente de un CLI (§3.1, `13-blocks.md` §4): el
mismo `HookRequest`, cada hook en su hilo con su tiempo (`timeout_ms` del hook, si no
`hook_timeout_ms`), una respuesta tarde, fallida o no declarada en `returns` cuenta como
nada (o como veto con `fail = closed`), cualquier veto gana y los contextos se suman. El
motor llama a `node/hook` de cada nodo cableado él mismo, con `ModuleCtx::call`: no pasa
por `hook.dispatch` porque eso es del socket, y el core sigue sin saber qué es un hook.

| Evento | Contexto | Veto |
|---|---|---|
| `session_start` | va al sistema durante toda la sesión | la sesión no arranca |
| `prompt` | va al sistema durante este turno | el turno falla (`failed` en la caja) y el prompt no entra en el historial |
| `pre_tool` | según `pre_tool_context`: `note` deja decidir a la política y el texto acompaña al resultado; `allow` la deja pasar; `ask` pregunta aunque la política diga otra cosa | la llamada no se hace; el modelo recibe `refused: <motivo>` |
| `post_tool` | se añade al resultado | ídem: el motivo se añade al resultado (ya se hizo) |
| `stop` | el turno sigue, con el texto como siguiente mensaje del usuario | igual que el contexto |

Un hook en un evento que el bucle no tiene (`pre_compact`: no compacta) se deja fuera con
aviso en `Started.warnings`.

**Permisos.** Sin hook que decida: `deny_tools` → no; `allow_tools` o un "permitir
siempre" anterior → sí; si no, `tool_policy`. Con `ask`, cada llamada sale como
`permission_request` (opciones `allow_once`, `allow_always`, `reject_once`) y espera a
`engine.session.decide`; un `cancel` del turno la cancela; pasado
`permission_timeout_seconds`, se rechaza. **Si la caja no puede preguntar** (su
`permissions` no es `ask`, p. ej. sin Decisor), `ask` toma la respuesta de la caja:
`allow` deja pasar y `reject` rechaza, con un aviso al arrancar. `cancel` corta también la
espera al modelo (la petición sigue en su hilo y su respuesta se tira); una herramienta o
un hook en curso acaban antes de que se note.

**Lo que se deja fuera, con aviso en `Started.warnings`:** el `provider` y las ranuras de
la caja (el motor pregunta a su propia entrada `model`), las skills, los montajes que no son
`mcp-stdio` o no arrancan (arriba), los hooks de la caja (son comandos de puente para un
CLI: cablea los nodos `hook@1` a `context`) y `resume`.

**Plantilla `templates/native-harness.toml`:** Agente ⇢ `native` ⇢ Proveedor (plantilla
`ollama` por defecto), `context` desde `files.document` (reglas de la casa, en
`session_start`) y `memory.inject` (en cada prompt), `tools` desde `memory.store` y
`documents.retrieve` (`search_documents`, `read_chunk`), y el Decisor de `human` para lo
que pregunta. Las herramientas de lectura están en `allow_tools`; el resto pregunta.

**Probado** contra un servidor falso con forma de OpenAI (y otro de Anthropic) en
`crates/modules/engine-native/tests/native.rs`: la herramienta corre por `tool.call` con
el nombre de quien llama y los eventos lo cuentan en orden; `ask` espera a `decide` y no
llama antes; `cancel` corta la espera; `deny` responde con un rechazo; `max_steps` y
`max_tool_calls` cortan; un hook `stop` mantiene el turno con su motivo; `prompt` y
`session_start` inyectan contexto; un veto `pre_tool` gana a `allow` y un contexto con
`pre_tool_context = allow` gana a `deny`; `history_turns` recorta; y la caja Agente lo
usa con `agent.prompt`. Montajes, contra un servidor MCP falso (el propio binario de test
sirviendo el servidor de `tools-mcp`): la herramienta montada sale en la lista, corre y su
respuesta vuelve al modelo con el binding del `env`; un nombre repetido va como
`box__echo`; `deny_tools` y un veto `pre_tool` la paran; `cancel` corta una llamada colgada y
`stop` mata el proceso; pasado `mount_call_timeout_ms` el modelo recibe un error; un montaje
que sale, uno que lista con error (con el binding oculto), uno `mcp-sse` y uno sin binario
quedan fuera con aviso y lo cableado sigue; y la caja Agente con un `toolset@1` cableado se
lo pasa al motor en su nombre. De punta a punta (`crates/regulus-cli/tests/
native_tools_mount.rs`): el binario real, caja `tools` + `tools-mcp` + `memory` en el
`tools` de la caja, motor nativo: el modelo falso escribe y lee la memoria por el puente
`regulus tools-mcp` y, al parar la ejecución, el puente termina. La plantilla, por CLI (`crates/regulus-cli/tests/
template_native_harness.rs`): aplica, `profile.check` sin diagnósticos, y un mensaje hace
que el modelo falso llame a `search_documents` sobre lo ingerido y conteste.

**Límites honestos:**

- `hook@1` no tiene respuesta "pregunta" ni "termina ya": un hook pide preguntar solo vía
  `pre_tool_context = ask` (un campo, no el hook), y un hook `stop` solo puede **seguir**
  (veto o contexto) o dejar acabar (nada); terminar antes de que el modelo acabe no se
  puede pedir desde un hook. Cambiarlo es `hook@2` o un campo opcional nuevo de `hook@1`.
- Sin streaming: el texto llega entero al final de cada llamada al modelo (un evento
  `text` por paso). Sin compactación: el contexto crece hasta `history_turns`.
- El historial vive en memoria de la sesión: no sobrevive al reinicio y no hay
  `load_session`. El historial de la caja (§5) sí.
- Sin reintentos ante errores del proveedor: el turno falla con el error (sin secretos).
- Delegación (`delegation.delegate`) y workflow como herramienta (`flow.tool`) aún no
  están en la plantilla: sus módulos no estaban en la rama; queda un TODO en su cabecera.
- Montajes: solo `mcp-stdio` y solo herramientas (ni recursos, ni prompts, ni sampling;
  una petición del servidor que no es `ping` recibe "method not found"). Sin
  `notifications/tools/list_changed`: se vuelve a listar en cada turno. Un montaje que muere
  a mitad de sesión no se rearranca: sus llamadas fallan con error para el modelo hasta la
  sesión siguiente. Las notificaciones de progreso se ignoran. El `stderr` del servidor se
  descarta, como el de los CLI.
- No probado contra un modelo real.
