# 04 — El cliente (shell visual)

> **v0.3** · 2026-10-02
> Las dos vistas, el protocolo que implican, y por qué el cliente se construye primero.
> Lee [`02-architecture.md` §1](02-architecture.md) (los dos límites) primero.

---

## 1. Por qué el cliente va primero

✅ **DECIDIDO** — Decisión de Alexander, y el límite externo de
[`02-architecture.md` §1](02-architecture.md) la hace correcta en vez de arriesgada.

El core es un servidor de socket. Por tanto el cliente se puede construir contra un
**mock server** que devuelve JSON enlatado — y **la lista de métodos de ese mock es la
especificación del core**.

```
   ui/  ──JSON-RPC──▶  mock-server (JSON enlatado)   ← fase 1: hoy
   ui/  ──JSON-RPC──▶  regulus-core (real)           ← fase 2: mismo cliente, sin cambios
```

El trabajo de UI produce la spec del core como **subproducto**, no como desecho. Dibujar
las vistas obliga a descubrir qué métodos hacen falta, en vez de adivinarlos.

### 1.1 La única regla que lo hace honesto

**El mock no inventa métodos cómodos para la UI.** Cada método tiene que ser algo que el
core pueda servir de verdad, con datos que un módulo realmente tenga.

Señal de alarma: si un método del mock devuelve un objeto perfectamente formado para un
componente concreto de la pantalla, probablemente es un método falso. Los métodos
devuelven **estado del sistema**, y la UI compone.

---

## 2. Las dos vistas

✅ **DECIDIDO** — **Dos proyecciones sobre un solo modelo de estado.** No son dos
aplicaciones. Alexander aceptó explícitamente el doble coste de UI.

- **OPERAR** — la vista por defecto, el día a día. Muestra **estado**: qué corre ahora.
- **TOPOLOGÍA** — el mismo estado desde otro ángulo. Muestra **cómo se enlazan** los
  módulos.

### 2.1 Vista OPERAR (por defecto)

```
┌──────────┬──────────────────────┬────────┐
│ MÓDULOS  │  Agentes             │ hcom   │
│ ● hcom   │ ┌──────────────────┐ │ ▸ log  │
│ ● roles  │ │ ◆ arch    fam: A │ │ ...    │
│ ● memoria│ │   mcp :3101   ●  │ │ ...    │
│ ○ mcp    │ ├──────────────────┤ │        │
│ + añadir │ │ ◆ impl    fam: A │ ├────────┤
├──────────┤ │   mcp :3102   ●  │ │MEMORIA │
│ PROYECTO │ ├──────────────────┤ │ 412    │
│ Regulus  │ │ ◆ test    fam: B │ │ notas  │
│ rama main│ │   mcp :3103   ●  │ │ global │
└──────────┴─└──────────────────┘─┴────────┘
          [ operar │ topología ]
```

Lectura:
- **Izquierda:** módulos con su estado real. `●` activo, `○` instalado sin usar nunca.
- **Centro:** agentes con familia y puerto MCP.
- **Derecha:** paneles contextuales del módulo seleccionado.
- **Siempre visible:** proyecto y rama.

### 2.2 Vista TOPOLOGÍA

```
┌────────────────────────────────────────┐
│  hcom ●──▶ Roles ●──▶ Memoria          │
│    └──────────────▶ MCP/agente         │
└────────────────────────────────────────┘
```

Lectura: quién provee a quién.

**Detalle crítico de diseño:** las aristas son **capacidades resueltas**
([`03-modules.md` §1](03-modules.md)), no cables dibujados a mano. La topología es
consecuencia de los manifests, no del ratón. No se arrastra para conectar: se declara en
el manifest y el grafo lo refleja.

Una arista puede estar en tres estados y el diseño debe distinguirlos:
- **resuelta** — alguien provee la capacidad
- **ausente opcional** — nadie la provee, `required = false`, el consumidor funciona igual
- **ausente requerida** — nadie la provee, `required = true`, el consumidor está `Disabled`

---

## 3. El protocolo que las vistas implican

🔶 **PROPUESTO** — Esta es la spec derivada de las dos vistas. Es el punto de partida de
`regulus-proto` y del mock server.

### 3.1 Métodos de lectura

| Método | Devuelve | Lo pide |
|---|---|---|
| `system.status` | qué corre: módulos activos, nº de agentes, proyectos abiertos | Barra permanente (requisito 5) |
| `project.list` | `[{ id, root, vcs: { branch } }]` — **todos** los abiertos (§8) | Selector de proyecto |
| `module.list` | `[{ id, name, state, last_used, origin.honors }]` | Panel MÓDULOS |
| `module.manifest(id)` | `provides[]`, `consumes[]` con `required` | Ficha de módulo, vista topología |
| `capability.graph` | `{ nodes: [module], edges: [{from, to, capability, status}] }` | Vista TOPOLOGÍA completa |
| `agent.list(project_id?)` | `[{ id, project_id, name, family, state, isolation }]` | Panel central |

Todo método que devuelva estado con alcance acepta `project_id` opcional. Sin él:
todos los proyectos. Con él: filtrado. Ver §8.

### 3.2 Métodos de stream

| Método | Entrega | Lo pide |
|---|---|---|
| `events.subscribe(pattern)` | eventos del bus (`agent.spawned`, `module.state_changed`) | Que la UI no haga polling |
| `terminal.subscribe(agent_id)` | salida de PTY del agente | Panel de log |

### 3.3 Lo que NO está en v1

`agent.spawn`, `agent.stop`, `module.install` — todo lo que **muta** estado. Razón: v1 es
la base sin funcionalidades ([`05-roadmap.md`](05-roadmap.md)). El cliente de v1 es de
solo lectura y eso ya prueba el protocolo completo de lectura y streaming.

### 3.4 Lo que esto revela sobre el core

Seis métodos de lectura y dos de stream. Y de ahí sale exactamente lo que `regulus-core`
tiene que tener en v1:

1. Un registro de módulos con estado y manifest → `module.list`, `module.manifest`
2. Un resolvedor de capacidades que sepa reportar su grafo → `capability.graph`
3. Una noción de proyecto → `project.current`
4. Un bus de eventos con suscripción → `events.subscribe`
5. Un registro de agentes (vacío en v1, pero con la forma correcta) → `agent.list`

Cinco cosas. Coincide con las cinco capacidades de
[`01-vision.md` §4.1](01-vision.md), que se derivaron por otro camino. Que las dos
derivaciones coincidan es la mejor señal de que el alcance de v1 está bien delimitado.

---

## 4. Estados vacíos

✅ **DECIDIDO** — Regla 3 de la matriz de ablación
([`03-modules.md` §4.1](03-modules.md)): **un módulo ausente muestra "no instalado", no
un error.**

El diseño tiene que cubrir estos cuatro casos como estados de primera clase, no como
excepciones:

| Situación | Qué muestra |
|---|---|
| Módulo no instalado | Panel con "módulo no instalado" y qué aportaría |
| Módulo `Disabled` | Qué capacidad `required` le falta, y quién la proveería |
| Módulo `Broken` | La razón del fallo, y un botón de recargar |
| Módulo `Idle`, `last_used: None` | Marcado visiblemente: instalado y nunca usado |

El último es el requisito 2 de [`01-vision.md` §5](01-vision.md). **Tiene que molestar un
poco a la vista** — es su función.

---

## 5. Orden de trabajo

1. `ui/` contra el mock server, vista OPERAR, solo lectura
2. Vista TOPOLOGÍA sobre `capability.graph` del mock
3. Los cuatro estados vacíos de §4
4. `regulus-proto` extraído del mock (los tipos ya existen de facto)
5. `regulus-core` implementa los 8 métodos; la UI no cambia una línea

El paso 5 es la prueba de que esto funcionó. Si la UI necesita cambios al conectar el
core real, algún método del mock era falso (§1.1).

✅ **Decisiones #7 y #8 cerradas.** Ya no bloquean:

- **Framework:** Tauri 2 + React 19 + Vite + Tailwind 4 + zustand
- **Transporte:** JSON-RPC 2.0 sobre WebSocket en `127.0.0.1`, token en `~/.regulus/token`
- **Terminal:** xterm.js 6 en el webview, `portable-pty` en el core
- **Grafo de topología:** `dagre` + SVG a pelo, **no** React Flow

Versiones y justificación completa en [`06-stack.md`](06-stack.md).

---

## 6. El handshake, antes que cualquier método

✅ **DECIDIDO** — Todo cliente negocia versión de protocolo **antes** de llamar nada. Si
el major no coincide, el cliente no arranca y dice qué versión necesita.

```json
→ {"method":"hello","params":{"client":"regulus-gui/0.2.0","proto":"1.3"}}
← {"result":{"core":"regulus/0.4.1","proto":"1.4","compatible":true}}
```

El mock server de la fase A **también** implementa `hello`. Es el primer método que se
escribe, no el último: así el cliente nunca se construye asumiendo que el core siempre
estará de acuerdo con él. Reglas completas en [`06-stack.md` §5.1](06-stack.md).

---

## 7. Agentes: Regulus es un ACP Client

✅ **DECIDIDO** en cuanto a la UI; la integración es la decisión #12.

Existe un protocolo estándar (**ACP**) que ~45 agentes ya hablan, incluidos Claude Agent,
Codex CLI, Gemini CLI, OpenCode, Cursor y Copilot. Cubre sesiones, tool calls,
terminales, permisos y planes de ejecución.

Consecuencia para el diseño de la UI, y es grande: **los paneles de agente no se diseñan
por agente.** Un tool call de Codex y uno de Gemini se pintan igual, porque llegan con la
misma forma. Lo mismo con planes, permisos y slash commands.

Lo que ACP **no** cubre es el eje agente↔agente — ahí es donde entra hcom absorbido.
Detalle en [`07-ecosystem.md` §1](07-ecosystem.md).

❓ **ABIERTO** — Si los paneles de ACP (plan, tool calls, elicitation) entran en v1 o
esperan. Recomendación: **esperan.** v1 es solo lectura y sin agentes reales; meterlos
ahora es diseñar contra datos imaginarios.

---

## 8. Multi-proyecto

✅ **DECIDIDO** (decisión #15) — **Varios proyectos simultáneos.** Agentes de distintos
repos vivos a la vez. Es consecuencia directa del principio **Flexible**
([`00-principles.md`](00-principles.md)): no cerrar la puerta.

Esto no es una feature de UI, es una propiedad del modelo. Consecuencias que hay que
respetar desde la primera línea de código:

| Dónde | Qué cambia |
|---|---|
| **Modelo de datos** | `project_id` en **todo** lo que tenga estado: agentes, mensajes, memoria, sesiones, entradas de terminal |
| **Métodos RPC** | Los de lectura aceptan `project_id` opcional. Sin él → todos los proyectos. Con él → filtrado. |
| **`project.current`** | Se sustituye por **`project.list`** + `project.active` (el que la UI tiene en foco) |
| **Store** | Un SQLite por proyecto, más el global. Ya estaba así en [`02-architecture.md` §6](02-architecture.md); ahora es obligatorio, no opcional |
| **Módulos** | Un módulo recibe el alcance en cada llamada. **Un módulo no puede tener "el proyecto actual" como estado propio** |
| **Bus de eventos** | Todo evento lleva `project_id`. Los suscriptores filtran. |
| **UI** | Conmutador de proyecto; y los paneles indican a qué proyecto pertenece lo que muestran |

**La regla que evita el error más probable:** ningún módulo guarda "el proyecto actual".
El proyecto activo es estado **de la UI**, no del core. El core sirve todos los proyectos
a la vez; cada cliente decide qué mira. Así dos clientes (escritorio y móvil) pueden estar
mirando proyectos distintos contra el mismo core.

**Actualización del panel PROYECTO** del mockup de §2.1: pasa a ser un selector con el
proyecto activo y los demás abiertos debajo.

---

## 9. Arranque del core

✅ **DECIDIDO** (decisión #16) — Por el principio **Flexible**, **los tres modos
funcionan**. Ninguno es obligatorio.

| Modo | Cómo | Para qué |
|---|---|---|
| **Auto-lanzar** (por defecto) | La app busca el core en el puerto; si no hay, lanza `regulus serve` desprendido y se conecta | Cero fricción: abres la app y funciona |
| **Manual** | `regulus serve` en una terminal; la app solo se conecta | Depurar: ves los logs y sabes qué core corre |
| **Servicio** | Unidad systemd de usuario; el core siempre vivo | Agentes de larga vida que sobreviven a reinicios de sesión |

Reglas que los hacen compatibles:

1. **La app nunca lanza un segundo core.** Si el puerto responde a `hello`, se conecta a
   ese. Siempre.
2. **El core que la app lanza se desprende.** Cerrar la ventana **no** mata el core ni los
   agentes. Esto ya lo exigía el reparto de la terminal
   ([`06-stack.md` §3](06-stack.md)); aquí se confirma.
3. **El auto-lanzado es configurable** — `autostart = false` para quien prefiera manual o
   servicio. Principio **Personalizable**.
4. Si el core no responde y el auto-lanzado está apagado, la app muestra el comando exacto
   a ejecutar. No un error genérico.

---

## 10. Idioma e internacionalización

✅ **DECIDIDO** (decisión #17) — **La UI va en inglés, con i18n desde el día 1.**

- **Inglés por defecto.** Los identificadores técnicos ya son inglés (`agent.list`,
  `memory.store`); mezclar idiomas en la misma pantalla es peor que elegir uno.
- **Todos los strings visibles salen a ficheros de traducción desde el principio.** Añadir
  i18n después obliga a tocar cada componente; hacerlo ahora cuesta casi nada.
- **La documentación sigue en español.** Es para Alexander y para los agentes, no es
  producto.

Regla que se deriva del principio **Flexible**: **ningún string visible se escribe
directamente en un componente.** Un literal en un `.tsx` es el mismo bug que un puerto
hardcoded en Rust.

---

## 11. Tema

✅ **DECIDIDO** (decisión #18) — **Solo oscuro visible, pero todos los colores pasan por
variables** desde la primera línea de CSS.

Añadir el tema claro luego es rellenar un segundo juego de valores, no refactorizar. Los
tokens y la paleta los define [`08-design-system.md`](08-design-system.md) — **ese
documento es el dueño de los colores**; no los dupliques aquí.

---

## 12. Payloads concretos — el contrato de la fase A

🔶 **PROPUESTO** — Esto es lo que el mock server devuelve, y por tanto lo que
`regulus-proto` tendrá que tipar. **Los datos de ejemplo están elegidos para cubrir todos
los estados que la UI debe pintar**, no para parecer bonitos: hay un módulo activo, uno
nunca usado, uno desactivado y uno roto.

### 12.1 `hello` — primero, siempre

```json
→ {"jsonrpc":"2.0","id":1,"method":"hello",
   "params":{"client":"regulus-gui/0.1.0","proto":"1.0"}}

← {"jsonrpc":"2.0","id":1,
   "result":{"core":"regulus/0.1.0","proto":"1.0","compatible":true}}
```

Si `compatible` es `false`, el resultado incluye `"required_proto":"2.0"` y el cliente
aborta con ese mensaje. No se degrada en silencio.

### 12.2 `system.status`

```json
{
  "core": "regulus/0.1.0",
  "uptime_s": 1284,
  "modules": { "active": 1, "idle": 1, "disabled": 1, "broken": 1 },
  "agents":  { "running": 2, "idle": 1, "total": 3 },
  "projects": { "open": 2 }
}
```

Alimenta la barra permanente (requisito 5 de [`01-vision.md` §5](01-vision.md)).

### 12.3 `project.list`

```json
[
  { "id": "prj_regulus", "root": "/home/artorias/Projectos/Regulus",
    "vcs": { "kind": "git", "branch": "master", "dirty": true } },
  { "id": "prj_hcom", "root": "/home/artorias/Projectos/hcom",
    "vcs": { "kind": "git", "branch": "main", "dirty": false } }
]
```

Dos proyectos a propósito: la UI tiene que nacer multi-proyecto (§8), no adaptarse después.

### 12.4 `module.list`

```json
[
  { "id": "hello", "name": "Hello", "version": "0.1.0",
    "state": { "kind": "active", "since": "2026-10-02T18:00:00Z" },
    "last_used": "2026-10-02T18:42:00Z", "origin": null },

  { "id": "memory", "name": "Memory", "version": "0.1.0",
    "state": { "kind": "idle", "last_used": null },
    "last_used": null, "origin": null },

  { "id": "agents", "name": "Agents", "version": "0.1.0",
    "state": { "kind": "disabled", "missing": ["agent.identity"] },
    "last_used": null,
    "origin": { "derived_from": "hcom", "author": "aannoo", "honors": "aannoo" } },

  { "id": "mcp", "name": "MCP", "version": "0.1.0",
    "state": { "kind": "broken", "reason": "panic in init: port 3101 in use" },
    "last_used": null, "origin": null }
]
```

`memory` con `last_used: null` es el caso del requisito 2: **instalado y nunca usado.**
Tiene que molestar un poco a la vista.

### 12.5 `module.manifest("agents")`

```json
{
  "id": "agents", "name": "Agents", "version": "0.1.0", "requires_proto": "^1",
  "provides": ["agent.source", "agent.spawn", "terminal.stream"],
  "consumes": [
    { "capability": "memory.store",   "required": false, "resolved_by": "memory" },
    { "capability": "agent.identity", "required": true,  "resolved_by": null }
  ],
  "origin": { "derived_from": "hcom", "author": "aannoo",
              "repo": "https://github.com/aannoo/hcom",
              "license": "MIT", "honors": "aannoo" }
}
```

### 12.6 `capability.graph`

```json
{
  "nodes": [
    { "id": "hello",  "state": "active" },
    { "id": "memory", "state": "idle" },
    { "id": "agents", "state": "disabled" }
  ],
  "edges": [
    { "from": "agents", "to": "memory", "capability": "memory.store",
      "status": "resolved", "required": false },
    { "from": "agents", "to": null, "capability": "agent.identity",
      "status": "missing_required", "required": true }
  ]
}
```

Los tres `status` posibles son `resolved`, `missing_optional` y `missing_required` — los
tres estados de arista de §2.2. `to: null` significa que nadie provee la capacidad.

### 12.7 `agent.list(project_id?)`

```json
[
  { "id": "agt_arch", "project_id": "prj_regulus", "name": "arch", "family": "A",
    "state": { "kind": "running" },
    "isolation": { "worktree": "/home/artorias/wt/arch",
                   "mcp": { "kind": "dedicated", "port": 3101 },
                   "memory": "project", "env": "inherit" } },

  { "id": "agt_impl", "project_id": "prj_regulus", "name": "impl", "family": "A",
    "state": { "kind": "idle" },
    "isolation": { "worktree": null,
                   "mcp": { "kind": "shared" },
                   "memory": "global", "env": "inherit" } }
]
```

⚠️ `isolation` se **sirve y se pinta** en la fase A, pero **no se implementa** en el core
hasta resolver la decisión #11 (cajas). La UI puede mostrarlo; nadie debe escribir la
lógica que lo aplica.

### 12.8 Notificaciones (sin `id`, el core las empuja)

```json
{"jsonrpc":"2.0","method":"event",
 "params":{"topic":"agent.spawned","project_id":"prj_regulus",
           "at":"2026-10-02T18:42:00Z","data":{"agent_id":"agt_test"}}}

{"jsonrpc":"2.0","method":"terminal.data",
 "params":{"agent_id":"agt_arch","seq":1042,"bytes":"<base64>"}}
```

`seq` es monótono por agente: deja detectar huecos al reconectar. `bytes` va en base64
mientras el transporte sea JSON; cuando pese, se pasa a frames binarios del mismo
WebSocket ([`06-stack.md` §4](06-stack.md)).

**Todo evento lleva `project_id`.** Los suscriptores filtran (§8).
