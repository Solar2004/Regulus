# AGENTS.md — Archivo maestro de Regulus

> **Este es el mapa. Léelo completo (~1.7k tokens) antes de abrir cualquier otro
> documento.** Describe qué contiene cada fichero para que no tengas que leerlos todos.
> Actualizado: 2026-11-08 · documentación vigente: ~48k tokens. Lo antiguo está en `docs/old/`.

---

## Qué es Regulus, en tres líneas

Un host de módulos con interfaz visual para construir, conectar y operar sistemas de
agentes de IA. Absorbe código ajeno y lo hace propio **sin perder la capacidad de
quitarlo**. El producto no son las features: es poder añadirlas y quitarlas sin que nada
se rompa.

**Estado:** la base funciona (`regulus serve`, CLI, módulo `hello`, matriz de ablación) y
el canvas carga del core real; la caja Agente corre Claude Code y opencode por ACP, con
proveedores (plantillas, ranuras, suscripción de Claude), skills, perfil aislado y
secretos, un canal de Comunicación entre agentes, y una TUI (`regulus` sin comando)
con la vista Operar; los agentes ya se envían mensajes por una herramienta MCP; el core
de la lógica de IA está (bus con dato, store SQLite, workflow como caja, `Call`), y la
caja Agente tiene la ranura `hooks` (puente `regulus hook`, módulo `hooks`); los doce
módulos de serie de [`docs/13-blocks.md`](docs/13-blocks.md) §3 están construidos
(lógica: `flow` · `files` · `process` · `judge` · `guards` · `triggers`; entorno:
`workspace` · `tasks` · `memory` · `web` · `human` · `service`); las plantillas se
aplican desde el core (`regulus template ls|show|apply`, `11-composition.md` §7).
La caja Agente se encadena con cables "luego" (`prompt` · `reply` · `failed`, contexto nuevo
por vuelta) y la primera plantilla, Ralph (`templates/ralph.toml`), corre con opencode.
**La app ya edita y opera** (2026-11-08, `plan.md` §3 filas 5-6, `04-client.md` §8-9): canvas
editable con paleta desde los manifiestos, Operar con ejecuciones y eventos reales, chat con
agentes, e instalar plantillas desde un formulario; Ajustes gestiona secretos, servicio y
módulos (`04-client.md` §10). **Los proyectos son carpetas con `regulus.toml`** (2026-11-08,
`11-composition.md` §13): perfiles y plantillas propios, veto de módulos por id, claves
desconocidas conservadas con aviso, `regulus project ls|show|open|close`. **La TUI edita
como la app** (2026-11-08, `04-client.md` §7): vistas Editar y Plantillas, Operar con eventos
y decisiones, teclas y números en un solo sitio. **Los clientes leen `[client]` de
`config.toml`** (2026-11-08, `04-client.md` §11): el core lo sirve como `config.client` con el
del proyecto encima y el origen de cada valor; la TUI toma teclas y números, la app los
valores de Operar y la primera vista. **Siete de las ocho plantillas de `13-blocks.md` §5
están construidas** (2026-11-08, datos TOML en `templates/`, sin Rust nuevo): Ralph, SDD,
evaluador-optimizador, best-of-N, equipo, research (cerrada de punta a punta con Claude) y
CLI chateable y pxl (Agente escribe la rejilla, un script la convierte en PNG, juez o
revisión humana; `65e0091`). **Las ocho están.** **Los huecos que las
plantillas anotaron están cerrados** (2026-11-08, `47d98bd`): entrada "luego" `workdir` en la
caja Agente, `value_path` en las reglas de `flow`, `timeout_ms` en la barrera de `join`, y
un turno colgado por el proveedor cierra en `failed` (`turn_timeout_ms`). **Los módulos
locales de un proyecto corren como procesos** (2026-11-08, `87dcee5`, `03-modules.md` §6,
`11-composition.md` §13): JSON-RPC por stdio, solo en proyectos de confianza
(`regulus project trust`), protocolo 1.14 con el estado `blocked`. **Release y
actualización** (2026-11-08, `e0dba00`, `06-stack.md` §5.4): `scripts/release.sh` saca
binario, AppImage y deb con manifiesto firmado (minisign); `regulus update` y Ajustes →
Updates instalan solo lo que esa firma avala. **Lo siguiente:** pulir con uso real. La
pasarela de proveedores queda aplazada. Plan y orden en
[`docs/plan.md`](docs/plan.md).

---

## ⚠️ Los principios mandan sobre todo lo demás

> **"Nuestro producto tiene que ser como el agua."** — Alexander

**Flexible · Modular · Rápido · Personalizable · Infraestructura.**

Cada principio tiene una prueba concreta que se puede fallar. **Lee
[`docs/00-principles.md`](docs/00-principles.md) (~2k tokens) antes de escribir código o
tomar una decisión de diseño.** Si una decisión de cualquier otro documento viola un
principio, el principio gana.

**Y la forma que toman: la caja líquida.** Toda pieza tiene forma fija por fuera y por
dentro se cambia: motor intercambiable, ranuras por propósito, envolturas en los cables,
se abre y se guarda como plantilla. Está en el mismo documento.

Las seis preguntas que se contestan antes de aprobar cualquier cambio:

1. ¿Hay algún literal que debería ser configuración? *(Flexible)*
2. ¿Sigue pasando la matriz de ablación? *(Modular)*
3. ¿Se midió, y entra en presupuesto? *(Rápido)*
4. ¿Se puede cambiar sin recompilar? *(Personalizable)*
5. ¿Existe el comando de CLI equivalente? *(Infraestructura)*
6. ¿Se puede abrir, cambiarle el motor y envolverla sin tocar el core? *(Caja líquida)*

Y el guardarraíl, que está en el mismo documento: **flexibilidad es no cerrar puertas, no
construir todas las habitaciones hoy.**

---

## Mapa de documentos

| Fichero | Tokens | Qué contiene |
|---|---|---|
| **`docs/plan.md`** | ~1.8k | **Si retomas en frío, empieza aquí.** Qué es base y qué es módulo, orden de trabajo, la pregunta abierta, lo rescatado de los docs antiguos (doctrina Predator, descartados, trampas) y cómo trabajar con Alexander. |
| **`docs/concept.md`** | ~1k | Qué es Regulus en llano: piezas (módulo, nodo, cable, workflow, perfil, ejecución) y las dos vistas. |
| **`docs/00-principles.md`** | ~2k | **Manda sobre todos los demás.** "Como el agua", **la caja líquida**, los 5 principios con su prueba, presupuestos de rendimiento, el guardarraíl. |
| **`docs/11-composition.md`** | ~5.5k | ✅ **El modelo**: base sin funciones, nodos, cables "usa" y "luego", workflows, perfiles, ejecuciones, secretos, instalar módulos. Respuestas de Alexander (§12). **Proyectos** como carpetas con `regulus.toml` (§13). |
| **`docs/12-agents.md`** | ~4.7k | ✅ **Agentes**: la caja Agente y sus motores (Claude Code, opencode…) como módulos, perfil seleccionable, canal ACP, el contrato `agent.engine@1`, **proveedores como caja líquida** (`llm.endpoint@1`, motores, plantillas, envolturas, pasarela, ranuras, suscripciones), perfil aislado y lo construido. |
| **`docs/13-blocks.md`** | ~8.5k | ✅ **Bloques**: la lógica de IA de serie. Qué pide el core (cable "luego" con dato, store, workflow como caja, sub-ejecución `Call`), la ranura `hooks`, los módulos `flow` · `files` · `process` · `judge` · `guards` · `triggers` · `workspace` · `tasks` · `memory` · `web` · `human` · `service`, las **plantillas** (pxl, Ralph, SDD, best-of-N, equipo, research, CLI custom), límites honestos y fuentes. |
| **`docs/02-architecture.md`** | ~4k | Los dos límites (socket / trait Rust), por qué módulos in-process, forma del workspace, **las abstracciones del core** (`Node`, `Port`, `Wire`, `Workflow`, `Profile`, `Execution`), bus y store. |
| **`docs/03-modules.md`** | ~2.2k | El contrato: trait `Module`, `ModuleCtx`, manifest con nodos/puertos/campos, workflow interno, estados, pánicos, **matriz de ablación**. |
| **`docs/04-client.md`** | ~4k | El cliente: el mock es la spec, Canvas + Operar, estados de primer nivel, `hello`, multi-proyecto, idioma y tema, la TUI (§7), Operar y canvas construidos (§8-9), Ajustes (§10), `[client]` de `config.toml` (§11). |
| **`docs/composition-preview.md`** | ~0.8k | Contrato actual del mock (métodos JSON-RPC) y límites del canvas A'/B'. |
| **`docs/06-stack.md`** | ~2.2k | Dependencias con versiones verificadas (único sitio), terminal, transporte, versionado. |
| **`docs/08-design-system.md`** | ~1.8k | Diseño de la app: Regulus Midnight, paleta y tokens (`assets/design/`), iconos de estado. Recetas en `centaury-recipes.md`. |
| **`docs/09-git.md`** | ~1.8k | Cómo se commitea: formato, tipos y scopes, ramas, push, **las 7 reglas para agentes**. Config en `.gitconventions`. |
| **`docs/10-docguard.md`** | ~1.4k | El harness que contrasta lo que haces con las reglas de los docs (`harness/docguard/rules.json`). |
| **`docs/old/`** | — | **Archivo del modelo anterior. No lo leas salvo que te lo pidan:** contradice lo vigente. |

---

## Rutas de lectura según tu tarea

| Tu tarea | Lee, en este orden | Coste |
|---|---|---|
| **Retomar en frío / no sé qué toca** | `00-principles.md` → `plan.md` | ~3.8k |
| **Entender el proyecto** | `concept.md` → `11-composition.md` | ~3.6k |
| **Tocar el core** | `02-architecture.md` → `03-modules.md` → `06-stack.md` §2.1 | ~5k |
| **Escribir un módulo** | `plan.md` §4.1 → `03-modules.md` | ~2.5k |
| **Tocar agentes, motores o proveedores** | `12-agents.md` → `03-modules.md` | ~6.9k |
| **Escribir un bloque de lógica o una plantilla** | `13-blocks.md` → `03-modules.md` | ~10.5k |
| **Diseñar o implementar UI** | `04-client.md` → `composition-preview.md` → `08-design-system.md` | ~3.5k |
| **Elegir una dependencia** | `plan.md` §4.3 (lo descartado) → `06-stack.md` | ~2.5k |
| **Commitear, ramificar o empujar** | `09-git.md` §1-5 | ~1.2k |
| **docguard te bloqueó** | `10-docguard.md` §2, §5 | ~0.6k |

---

## Reglas duras

### 1. La convención de marcas

| Marca | Qué hacer |
|---|---|
| ✅ **DECIDIDO** | Constrúyelo. No lo cuestiones ni lo "mejores". |
| 🔶 **PROPUESTO** | Hay recomendación con razonamiento. Necesita aprobación de Alexander. |
| ❓ **ABIERTO** | **No lo inventes.** Pregunta. |

### 2. No empieces nada bloqueado

- **Aislamiento de ejecución** → fuera de v1 (`11-composition.md` §12)
- **Absorber hcom o tocar `~/Projectos/hcom`** → espera la base y la matriz de ablación

### 3. Los dos invariantes que no se negocian

1. **Quitar cualquier módulo deja el resto funcionando.** Verificable con la matriz de
   ablación (`03-modules.md` §4). No es una intención.
2. **El core nunca conoce un módulo por su nombre**, solo por capacidad. Si escribes
   `if module.id == "agents"` en el core, está mal.

### 4. La regla de alcance

Toda pieza de la base debe estar justificada por un módulo de `plan.md` §2 que la pida.
**Si ningún módulo la pide, no se construye.** El riesgo nº1 del proyecto es construir el
framework y no llegar nunca a las features.

### 5. Un solo sitio por dato

Las versiones de dependencias viven **solo** en `06-stack.md`. Si duplicas, se
desincroniza. Si encuentras una duplicación, bórrala y deja un enlace.

### 6. El código va en inglés

Todo lo que forma parte de la app (`ui/`, `crates/`, `src-tauri/`) se escribe en
**inglés**: comentarios, identificadores, logs, mensajes de error y commits. Los textos
visibles van en los ficheros de traducción (`04-client.md` §6). **Solo los docs van en
español.** Decisión #20. docguard lo vigila.

### 7. Cómo se commitea

El repo tiene hooks: un mensaje mal formado **no entra**. Lo mínimo que necesitas saber
antes de tu primer commit — el resto está en [`docs/09-git.md`](docs/09-git.md):

```bash
scripts/install-hooks.sh --check    # ¿hooks activos en este clon? si no: scripts/install-hooks.sh
```

```
tipo(scope): asunto en imperativo, minúscula, sin punto, ≤72 caracteres

El por qué. El qué está en el diff.

Refs: docs/plan.md §3
Co-Authored-By: Tu nombre <email>
```

- Tipos y scopes válidos: [`.gitconventions`](.gitconventions). Scope nuevo → se añade ahí
  en el mismo commit que lo usa.
- **Nada de `--no-verify`.** Si una regla te estorba, propón el cambio en
  `.gitconventions`; `pre-push` revalida el rango entero de todos modos.
- **No empujes a un remoto ni crees tags sin que te lo pidan.** Commitear es reversible;
  publicar no.
- Un commit por incremento verificado, y solo lo que te pidieron tocar.

---

## Lo que toca ahora

| Prioridad | Qué | Bloqueado por |
|---|---|---|
| 1 | **Pulir con uso real**: mostrar el estado `blocked` con su acción "confiar" en Ajustes; instalar una actualización de la app con la app abierta (dos versiones publicadas) | nada |
| 1b | **Plantillas** de `13-blocks.md` §5: ~~mecanismo~~, ~~Ralph~~, ~~SDD~~, ~~evaluador~~, ~~best-of-N~~, ~~equipo~~, ~~research~~, ~~CLI chateable~~, ~~pxl~~. **Las ocho hechas** (2026-11-08) | — |
| 2 | **App completa**: ~~solape de nodos por clic~~, ~~pie "No modules installed"~~, ~~`pnpm tauri:dev`~~, ~~Ajustes: secretos, servicio y módulos~~ (`04-client.md` §10), ~~proyectos con `regulus.toml`~~ (`11-composition.md` §13), ~~TUI que edita como la app~~ (`04-client.md` §7), ~~`[client]` de `config.toml` leído por app y TUI~~ (`04-client.md` §11), ~~`ProcessModule` para `[[modules.local]]`~~ (`03-modules.md` §6); ~~release + `regulus update` + Ajustes → Updates~~ (`06-stack.md` §5.4); lo que queda está en la fila 1 | nada |
| 3 | **Comunicación como caja**, pasos 3-5 (`11-composition.md` §3.0) | nada |
| — | Pasarela, Router, Traductor, motor Local (`12-agents.md` §7) | aplazado: la base de proveedores y skills basta |

---

## Contexto del entorno

- Alexander trabaja en **Arch Linux**, kitty + tmux. Proyectos en `~/Projectos/`.
- **Responde en español, directo y sin rodeos.**
- `~/Projectos/hcom` es su fork propio (`Solar2004/hcom`) — modificable, pero **no
  todavía**.
- El repo es git (`master`).
