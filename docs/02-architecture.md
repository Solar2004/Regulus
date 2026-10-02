# 02 — Arquitectura

> **v0.3** · 2026-10-02
> Estructura del core, los dos límites, el workspace y el stack.
> Lee [`01-vision.md`](01-vision.md) primero.

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
        │ regulus-core │   registro · bus de eventos · store
        └──────┬───────┘
               │
               │  trait Module (Rust, in-process)  ← LÍMITE INTERNO
               │
   ┌───────────┼───────────┬────────────┬──────────┐
 agents      memory      roles      families     mcp     ← módulos = crates
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
una llamada vino de la GUI, del CLI o de otro módulo. `agent.list` es `agent.list` en
las dos fronteras.

---

## 2. Por qué los módulos son in-process

✅ **DECIDIDO** — La decisión con más consecuencias del proyecto, así que el
razonamiento completo:

IPC, serialización y versionado de protocolo existen para cruzar una **frontera de
confianza**. En Regulus esa frontera no existe: los módulos son código de Alexander, en
Rust, reescrito para encajar ([`01-vision.md` §2](01-vision.md)). Pagar el coste de IPC
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
│  ├─ regulus-cli/        # regulus serve · module ls · agent ls
│  ├─ regulus-tauri/      # app de escritorio: un cliente más
│  └─ modules/
│     ├─ hello/           # módulo trivial de prueba
│     ├─ agents/          # (futuro) de hcom, en honor a aannoo
│     ├─ roles/
│     ├─ families/
│     ├─ memory/
│     └─ mcp/
├─ ui/                    # frontend web del shell
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

✅ **DECIDIDO** — Se **definen** en v1 aunque no se implementen features sobre ellas.
Alexander: *"establecer cosas como digamos Agentes, si existirán, establecer que podrán
tener aislamiento, sistemas, etc., pero hacer de momento la base"*.

```rust
/// La abstracción central.
/// Los módulos le cuelgan cosas sin conocerse entre ellos.
pub struct Agent {
    pub id:        AgentId,
    pub project:   ProjectId,
    pub isolation: Isolation,
    pub state:     AgentState,
}

pub enum AgentState {
    Spawning,
    Running,
    Idle,
    Stopped,
    Failed { reason: String },
}

/// Qué significa "aislado" se decide POR AGENTE, no globalmente.
pub struct Isolation {
    pub worktree: Option<PathBuf>,  // ¿su propio worktree de git?
    pub mcp:      McpScope,         // None | Shared | Dedicated { port }
    pub memory:   MemoryScope,      // None | Project | Global
    pub env:      EnvScope,         // Inherit | Clean | Explicit(map)
}

pub struct Project {
    pub id:   ProjectId,
    pub root: PathBuf,
    pub vcs:  Option<VcsInfo>,
}
```

**Decisión #15: varios proyectos simultáneos.** El core sirve **todos** los proyectos
abiertos a la vez. Dos consecuencias que son reglas, no sugerencias:

1. **`project_id` aparece en todo lo que tenga estado** — agentes, mensajes, memoria,
   sesiones, eventos del bus.
2. **Ni el core ni ningún módulo guarda "el proyecto actual".** El proyecto activo es
   estado **del cliente**. Así dos clientes (escritorio y móvil) pueden mirar proyectos
   distintos contra el mismo core.

Detalle completo en [`04-client.md` §8](04-client.md).

**Por qué `Isolation` se define completa en v1 aunque nadie aplique sus campos:**
retrofitear aislamiento a posteriori es el cambio más caro que existe, porque toca todos
los módulos a la vez. Definirla ahora cuesta una tarde; añadirla en v3 cuesta un
rediseño.

❓ **ABIERTO** — `Isolation` es exactamente el territorio de la idea de "cajas" de
Alexander. Ver [`ideas/boxes.md`](ideas/boxes.md). Si las cajas se adoptan, `Isolation`
probablemente se convierte en un caso particular de caja, no en un struct fijo. **No
implementar `Isolation` hasta decidir eso.**

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
| PTY | `portable-pty`, **del lado del core** |
| Integración con agentes | `agent-client-protocol` — ver [`07-ecosystem.md`](07-ecosystem.md) |
| MCP | `rmcp` |
| Store | `rusqlite` |

### 5.4 Transporte

✅ **DECIDIDO** (decisión #8) — JSON-RPC 2.0 sobre WebSocket en `127.0.0.1`, con token en
`~/.regulus/token`. Detalles y justificación en [`06-stack.md` §4](06-stack.md).

---

## 6. Bus de eventos y store

✅ **DECIDIDO**

**Bus:** estado observable sin polling. `agents` publica `agent.spawned`; `roles`,
`families` y la UI reaccionan **sin que `agents` sepa que existen**. El bus también es
lo que alimenta la suscripción de los clientes por socket.

**Store:** un SQLite por proyecto + uno global. Cada módulo obtiene un **namespace
propio**. Leer de otro namespace solo a través de capacidades declaradas. Evita que el
estado compartido degenere en variable global donde todos escriben.

❓ **ABIERTO** — Esquema concreto del store y si el bus es síncrono o `tokio::broadcast`.
Se decide al escribir `regulus-core`, no antes.
