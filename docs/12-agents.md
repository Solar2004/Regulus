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
anterior; en el del usuario no escribe nada y avisa. El puente lee el evento por stdin,
pregunta al core (`hook.dispatch`, que reenvía `node/hook` a los nodos conectados con su
tiempo límite) y responde como el CLI lee los hooks.

| Motor | Hooks |
|---|---|
| Claude Code | Regulus es dueño de la clave `hooks` del `settings.json` del perfil aislado (el que lee con `settingSources: ["user"]`). Eventos: `pre_tool` → `PreToolUse`, `post_tool` → `PostToolUse`, `stop` → `Stop`, `session_start` → `SessionStart`, `prompt` → `UserPromptSubmit`, `pre_compact` → `PreCompact`; uno desconocido queda fuera con aviso. Matcher solo en herramientas y solo en su forma literal (`Bash\|Write`); si no, Claude pregunta por todas y decide el nodo. `timeout` = el del hook + 1 s. Puente: veto → exit 2 con el motivo en stderr; contexto → `hookSpecificOutput.additionalContext` (en `PreCompact`, stdout plano: instrucciones del resumen); nada → exit 0; core inalcanzable → exit 1 (error no bloqueante). Un veto en `post_tool` o `session_start` no para nada (Claude solo lo muestra): aviso al arrancar. Esquema: https://code.claude.com/docs/en/hooks |
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
`stop_reason` + `text`), `engine.session.status` (`idle`, `busy`, `ended`, lo que ACP sabe del turno; opcional) y `engine.session.stop`. `StartArgs.tools` lleva los montajes de herramientas (`11-composition.md` §3.0). Cambiar su forma es `agent.engine@2`. Los
contratos de capacidad viven en `crates/contracts/<capacidad>`, no dentro de ningún módulo.

**Cada motor anuncia además su propio método de detección** (`claude.detect`,
`opencode.detect`), para que la UI liste las instalaciones sin arrancar nada. Detectado en
la máquina de Alexander: `claude`, `claude-sub-bin` y `claude-ultra-bin` (cada uno con su
versión), y los dos `opencode`.

**Permisos:** cada caja elige `reject` (por defecto) o `allow`. Preguntar a una persona
llega con el bus, que es quien puede llevar la pregunta hasta un cliente.

**Probado de punta a punta** con `regulus run agents` (`crates/modules/agent/examples/`):
las dos cajas arrancan, responden, conservan el contexto entre prompts y, al parar,
no queda ningún proceso vivo. Con `--without regulus/engine-claude`, la caja de Claude
queda desactivada y la de opencode sigue funcionando.

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
