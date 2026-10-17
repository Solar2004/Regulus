# 02 — Arquitectura

> **v0.4** · 2026-10-05 · aplica el modelo de composición
> ([`11-composition.md`](11-composition.md)): el agente sale del core, todo son nodos y cables.
> Estructura del core, los dos límites, el workspace y el stack.
> Lee [`plan.md`](plan.md) primero.

---

## 1. Los dos límites

✅ **DECIDIDO** — La clave del diseño: hay **dos fronteras distintas**, con tecnologías
distintas. Confundirlas es el error principal a evitar.

```
   GUI        CLI        móvil / SSH        ← clientes
    └──────────┴─────────────┘
               │
               │  JSON-RPC sobre socket     ← LÍMITE EXTERNO (red)
               │
        ┌──────▼───────┐
        │ regulus-core │   registro · cables · bus · store · ejecuciones
        └──────┬───────┘
               │
               │  trait Module (Rust, in-process)  ← LÍMITE INTERNO
               │
   ┌─────────────┬─────────┼─────────┬───────────┐
 comunicación  agente  proveedores  modelos   procesos   ← módulos = crates que traen nodos
```

### Límite externo — core ↔ clientes

**Socket local, JSON-RPC.** Decidido el día 1, no retrofitado.

Qué compra:

1. La GUI es un cliente más → desechable y reemplazable sin tocar el core.
2. Soportar móvil o SSH luego es **añadir un cliente**, no reescribir.
3. El CLI puede probar el core completo sin que exista UI.
4. Un cliente puede estar escrito en cualquier lenguaje.

### Límite interno — core ↔ módulos

**Trait Rust, in-process.** Cero serialización, type-safe, y el core no conoce ningún
módulo por nombre — solo por capacidad. El contrato está en
[`03-modules.md`](03-modules.md).

### La regla que une los dos

**Los nombres de método son los mismos a ambos lados del core.** Un módulo no sabe si
una llamada vino de la GUI, del CLI o de otro módulo. `module.list` es `module.list` en
las dos fronteras.

---

## 2. Por qué los módulos son in-process

✅ **DECIDIDO** — La decisión con más consecuencias del proyecto, así que el
razonamiento completo:

IPC, serialización y versionado de protocolo existen para cruzar una **frontera de
confianza**. En Regulus esa frontera no existe: los módulos son código de Alexander, en
Rust, reescrito para encajar ([`plan.md` §4.1](plan.md), doctrina Predator). Pagar el coste de IPC
sería plumbing sin nada a cambio.

| Argumento a favor de procesos externos | Por qué no aplica aquí |
|---|---|
| "Módulos en cualquier lenguaje" | Todo es Rust de todas formas; hcom ya lo es |
| "Un módulo que crashea no tumba el host" | Se mitiga con `catch_unwind` en el borde de `Module::call` |
| "Hot-reload sin recompilar" | Build incremental de un crate = segundos |
| "hcom se enchufa sin tocarlo" | **Falso por diseño**: la doctrina Predator exige tocarlo |

**WASM queda descartado** por incompatibilidad técnica directa, no por moda: los cinco
casos de uso necesitan PTYs, procesos hijos y sockets arbitrarios. Un sandbox WASM con
todos esos agujeros ya no es un sandbox.

**Precio aceptado:** recompilar el workspace al añadir un módulo, y que un `panic` no
contenido mate el proceso.

**La puerta que queda abierta:** si algún día hace falta un módulo en otro lenguaje, se
escribe un `ProcessModule` que implementa el mismo trait y habla por stdio. El core no
cambia. No se paga por ello hoy, pero no se cierra.

---

## 3. Forma del repositorio

✅ **DECIDIDO**

```
Regulus/
├─ crates/
│  ├─ regulus-core/       # registro, bus, trait Module, store. SIN UI.
│  ├─ regulus-proto/      # tipos del contrato (core ↔ clientes ↔ módulos)
│  ├─ regulus-cli/        # regulus serve · module ls · graph · run · agent prompt
│  ├─ regulus-acp/        # canal ACP + kit que comparten los motores de agente
│  ├─ contracts/          # formato de cada capacidad (agent-engine/ = agent.engine@1)
│  ├─ regulus-tauri/      # app de escritorio: un cliente más
│  │  ├─ frontend/        # React+Vite: lo que se ve (compila a frontend/dist)
│  │  └─ src-tauri/       # Cargo, tauri.conf.json, ventana nativa
│  └─ modules/
│     ├─ hello/           # módulo trivial de prueba
│     ├─ communication/   # de hcom, en honor a aannoo
│     ├─ agent/           # la caja Agente (12-agents.md)
│     ├─ engine-claude/   # un motor por CLI
│     ├─ engine-opencode/
│     ├─ providers/
│     ├─ models/
│     └─ processes/       # procesos y PTY
├─ docs/
├─ README.md
└─ Cargo.toml             # workspace
```

Por qué el core es un crate sin UI y no un binario único:

1. "Infraestructura donde trabajará mi código" implica que Regulus sirve también sin
   ventana abierta: desde CLI, desde CI, por SSH.
2. Hace comprobable la métrica norte: si `regulus-cli` carga un módulo, el contrato es
   real y no un atajo de la UI.
3. La UI es la pieza con más probabilidad de ser reemplazada. Como cliente, es
   desechable sin drama.

---

## 4. Las abstracciones del core

✅ **DECIDIDO** (2026-10-05, [`11-composition.md`](11-composition.md)) — **El core no
tiene funciones propias.** No sabe qué es un agente, un modelo ni una skill. Solo conoce
piezas genéricas: nodos, puertos, cables, workflows, perfiles y ejecuciones.

> **Corrección (v0.4):** hasta v0.3 aquí vivían `Agent` e `Isolation`. **Salen del
> core:** `Agent` es un nodo del módulo agente; `Isolation` pasa a ser los límites de un
> grupo de nodos (`11` §9.1); procesos y PTY son el módulo procesos.

Forma, no código final. Los nombres de campo coinciden con el mock
(`frontend/src/composition/model.ts`):

```rust
/// Una entrada o salida de un nodo.
pub struct Port {
    pub id:          PortId,
    pub capability:  PortType,     // tipo con versión: "llm.chat@1"
    pub direction:   Direction,    // Input | Output
    pub kind:        WireKind,     // Uses | Then
    pub required:    bool,         // por defecto false (03-modules.md §1.1)
    pub cardinality: Cardinality,  // One | Many — por defecto One
}

pub struct Node {
    pub id:                NodeId,
    pub node_type:         String,             // lo declara el módulo
    pub module:            Option<ModuleId>,   // "autor/módulo@versión"
    pub resource:          Option<ResourceRef>,// skill, prompt, doc: ficheros
    pub ports:             Vec<Port>,
    pub config:            Map<String, Value>, // secretos solo como "secret:<nombre>"
    pub internal_workflow: Option<WorkflowId>,
    pub exposed:           Vec<String>,        // lo que se edita sin recompilar
}

/// Uses = servicio permanente. Then = suscripción al bus.
pub struct Wire { pub id: WireId, pub kind: WireKind, pub from: PortRef, pub to: PortRef }

pub struct Workflow {
    pub id:     WorkflowId,
    pub kind:   WorkflowKind,          // Main | Extra | Module
    pub module: Option<ModuleId>,
    pub nodes:  Vec<Node>,
    pub wires:  Vec<Wire>,
}

pub struct Profile {
    pub id:            ProfileId,
    pub project:       Option<ProjectId>,  // None = perfil global
    pub extends:       Option<ProfileId>,  // reservado: herencia fuera de v1
    pub main_workflow: WorkflowId,
    pub workflows:     Vec<Workflow>,
}

/// Una copia viva de un workflow. Puede haber varias del mismo a la vez.
pub struct Execution {
    pub id:       ExecutionId,
    pub workflow: WorkflowId,
    pub project:  Option<ProjectId>,
    pub state:    ExecutionState,      // Starting | Running | Stopped | Failed { reason }
}

pub struct Project {
    pub id:   ProjectId,
    pub root: PathBuf,
    pub vcs:  Option<VcsInfo>,
}
```

Las posiciones y el layout del canvas viajan con el workflow, pero el core no los
interpreta: son del cliente.

**Reglas de los cables** (`11` §4):

1. **Uses** une una salida con una entrada del **mismo tipo y versión**. Si la entrada es
   `One`, admite un cable; si es `Many`, varios.
2. **Then** es una suscripción al bus, no un motor de flujo. El core corta una cadena al
   pasar de una profundidad **configurable** y lo reporta (bucles A→B→A).
3. **Autoconexión:** si para una entrada hay exactamente una salida compatible, se conecta
   sola.

**Qué deja la base abierta a propósito** (`11` §12): cuántos cables admite cada entrada
(el campo existe, por defecto `One`) y qué lleva cada tipo de puerto (el tipo tiene
versión; el formato lo fija el primer módulo que lo use).

**Decisión #15: varios proyectos simultáneos.** El core sirve **todos** los proyectos
abiertos a la vez. Dos consecuencias que son reglas, no sugerencias:

1. **`project_id` aparece en todo lo que tenga estado** — perfiles, ejecuciones, eventos
   del bus, store.
2. **Ni el core ni ningún módulo guarda "el proyecto actual".** El proyecto activo es
   estado **del cliente**. Así dos clientes (escritorio y móvil) pueden mirar proyectos
   distintos contra el mismo core.

Detalle en [`04-client.md` §5](04-client.md).

❓ **ABIERTO** — Aislamiento **de ejecución** (`11` §9, nivel 2): fuera de v1. No se
implementa; el modelo no lo impide.

---

## 5. Stack

### 5.1 Core, CLI y módulos

✅ **DECIDIDO** — **Rust.** Razón no sentimental: las cinco capacidades que el core
necesita son todas trabajo de sistema (procesos, PTYs, SQLite, sockets), y la primera
habilidad a consumir (hcom) ya es Rust, así que el código se transplanta en vez de
traducirse.

### 5.2 GUI

✅ **DECIDIDO** (decisión #7) — **Tauri 2.** El argumento decisivo es la terminal: solo
el webview tiene un emulador probado en batalla (xterm.js). El razonamiento completo, la
tabla comparativa y las versiones concretas están en
[`06-stack.md` §1](06-stack.md).

El riesgo de WebKitGTK es aceptable **porque Regulus no necesita navegador embebido**
(ese era requisito de Orca, no de Regulus).

Nota: por el límite externo de §1, esta decisión es **reversible**. Cambiar de Tauri a
egui es escribir otro cliente, no reescribir Regulus. Es la decisión barata del
proyecto, no la caja fuerte.

### 5.3 Dependencias concretas

Todas las versiones verificadas viven en **[`06-stack.md`](06-stack.md)**. No las
dupliques aquí: un solo sitio con las versiones, o se desincronizan.

Resumen de lo que importa saber desde arquitectura:

| Capa | Elección |
|---|---|
| RPC del límite externo | `jsonrpsee` (JSON-RPC 2.0 sobre WebSocket) |
| PTY | `portable-pty`, en el proceso del core (módulo procesos) |
| Integración con agentes | `agent-client-protocol` — ver [`12-agents.md`](12-agents.md) |
| MCP | `rmcp` |
| Store | `rusqlite` |

### 5.4 Transporte

✅ **DECIDIDO** (decisión #8) — JSON-RPC 2.0 sobre WebSocket en `127.0.0.1`, con token en
`~/.regulus/token`. Detalles y justificación en [`06-stack.md` §4](06-stack.md).

---

## 6. Bus de eventos y store

✅ **DECIDIDO**

**Bus:** estado observable sin polling. Comunicación publica `message.received`; un cable
"luego" lo lleva a Entregar y la UI reacciona, **sin que Comunicación sepa quién
escucha**. Los cables "luego" corren sobre el bus. También alimenta la suscripción de los
clientes por socket.

**Store:** un SQLite por proyecto + uno global. Cada módulo obtiene un **namespace
propio**. Leer de otro namespace solo a través de capacidades declaradas. Evita que el
estado compartido degenere en variable global donde todos escriben.

❓ **ABIERTO** — Esquema concreto del store y si el bus es síncrono o `tokio::broadcast`.
Se decide al escribir `regulus-core`, no antes.
