# 12 — Agentes: la caja y sus motores

> **v1.0** · 2026-10-17 · Respuesta de Alexander a la pregunta abierta de `plan.md` §2,
> más la investigación y la medición del mismo día. Manda sobre `old/07-ecosystem.md`.

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
| **Caja Agente** | El nodo que ve el usuario. Entradas genéricas: proveedor, skills, MCP, mensajes | módulo `agent` |
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

Hoy existe el **existente** (campo `config_dir`; vacío = el setup por defecto del usuario).
El **aislado** llega con Proveedores: sin proveedor conectado, un perfil vacío no tiene con
qué autenticarse.

| Entrada genérica | Motor Claude Code | Motor opencode |
|---|---|---|
| Instalación | `CLAUDE_CODE_EXECUTABLE` | binario elegido |
| Perfil | `CLAUDE_CONFIG_DIR` | su config |
| Proveedor | `providers/set` de ACP: `anthropic`, `bedrock` o `vertex` + `baseUrl` | proveedor en su config |
| Skills | se colocan en el perfil | por verificar |
| MCP | `mcpServers` al crear la sesión | igual |

**Un proveedor que no habla el protocolo del motor** (p. ej. solo OpenAI hacia Claude)
se resuelve con un **nodo traductor** en el cable "usa": Agente ⇢ Traductor ⇢ Proveedor.
Es la lectura A de las cajas (`11-composition.md` §9.1). Es lo que hoy hace `token-proxy`.

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
`stop_reason` + `text`) y `engine.session.stop`. Cambiar su forma es `agent.engine@2`. Los
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

## 6. Orden resultante

Sustituye a las dos ramas de `plan.md` §2:

1. Caja **Agente** + **motor opencode** + **motor Claude** (canal ACP)
2. **Proveedores** (se enchufan a la caja)
3. **Skills**
4. **Comunicación** (hcom, sobre sesiones ACP: `old/07-ecosystem.md` §1.4)

Precedente más cercano: vibe-kanban (Rust) mezcla canales por agente (stream-json,
`app-server`, `serve`, ACP) y anuncia su cierre. No absorber sin la matriz de ablación.
