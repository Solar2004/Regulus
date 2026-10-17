# Plan — qué se construye y en qué orden

> **v1.0** · 2026-10-05 · ✅ **DECIDIDO por Alexander** — sustituye a `old/05-roadmap.md`
> y `old/HANDOFF.md`. **Si retomas en frío, empieza aquí.**

---

## 1. Lo que es Regulus: la base

La base **no sabe nada de IA**. Solo hace que las piezas encajen. Son seis cosas:

| # | Pieza | Qué hace |
|---|---|---|
| 1 | **Registro** | Cargar módulos; ponerlos y quitarlos sin que nada se rompa |
| 2 | **Nodos y cables** | Guardar en un fichero qué piezas hay y cómo se conectan |
| 3 | **Bus** | "Pasó esto" → la pieza conectada reacciona |
| 4 | **Store** | Guardar datos: un cajón por módulo y por proyecto |
| 5 | **Ejecuciones** | Encender y apagar workflows, varios a la vez |
| 6 | **Servidor** | La app, el CLI y el móvil se conectan a él |

La app (Canvas + Operar) y el CLI son **clientes** de la base, no parte de ella.
Modelo técnico: [`11-composition.md`](11-composition.md) y
[`02-architecture.md`](02-architecture.md).

---

## 2. Lo que son módulos

Se enchufan después, por el mismo camino que cualquier otro módulo:

| Módulo | Qué hace | Equivale a |
|---|---|---|
| **Terminal / procesos** | Lanzar Claude Code, opencode… en terminales | lo que hoy se hace en tmux |
| **Comunicación** | Que los agentes se hablen entre ellos | **hcom** |
| **Proveedores** | Conectar OpenAI, Anthropic, modelos locales | API keys |
| **Agente** | Un agente propio de Regulus que usa un proveedor | — |
| **Modelos** | Elegir o rotar modelos | — |

Más adelante, también como módulos: roles, familias, MCP aislado, memoria.

✅ **DECIDIDO (2026-10-17) — el orden de los módulos.** Regulus no tiene agentes propios:
el nodo Agente es una caja y el CLI (Claude Code, opencode…) es su **motor**, cada uno
un módulo. Orden: Agente + motores → Proveedores → Skills → Comunicación. Detalle en
[`12-agents.md`](12-agents.md).

---

## 3. Orden de trabajo

| # | Qué | Est. | Estado |
|---|---|---|---|
| 1 | Mock + canvas de solo lectura (A'/B') | — | ✅ hecho |
| 2 | Modelo de composición aprobado y aplicado a `02`/`03` | — | ✅ hecho |
| 3 | **La base**: `regulus-proto`, `regulus-core`, `regulus serve` con `hello` y los métodos del mock | 2-3 días | ✅ hecho (2026-10-17) |
| 4 | Primer módulo (caja Agente + motores, `12-agents.md` §5) + módulo `hello` + **matriz de ablación** | 1-2 días | ✅ hecho (2026-10-17) |
| 5 | Canvas editable: crear nodos, tirar cables, guardar | ~1 sem | — |
| 6 | Vista Operar con ejecuciones reales | ~1 sem | — |

**Prueba de la base (paso 3):** la UI apunta al core real con `VITE_RPC_URL` y funciona
sin cambiar una línea.

**Hcom no se absorbe antes de que exista la matriz de ablación** (paso 4). Absorber un
módulo grande sin ella es cómo se pierde la modularidad sin enterarse.

**Falta especificar antes de que haga falta:** el formato de `regulus.toml`, los atajos de
teclado (reconfigurables) y qué pasa al reconectar tras caerse el WebSocket.

---

## 4. Lo rescatado de los docs antiguos

### 4.1 La doctrina Predator

El código ajeno (hcom) **se absorbe y se reescribe** como módulo propio de Regulus: no se
lanza un binario de otro, no hay `hcom --flags` por debajo. Pero **absorbido no es
fundido**; un módulo absorbido siempre:

1. Vive en su propio crate.
2. El core no lo conoce por nombre, solo por capacidad.
3. Regulus arranca sin él y el resto funciona.

Si deriva de código ajeno, su manifest lleva `[module.origin]` con autor, repo, licencia y
`honors` (hcom → aannoo).

### 4.2 Qué NO es Regulus

| No es | Por qué |
|---|---|
| Un editor de código | Zed lo hace mejor |
| Un wrapper de Claude Code | Claude Code es un módulo posible, no el cimiento |
| Un clon de Orca | Orca tiene opiniones fijas; Regulus es una base |

### 4.3 Descartado — no lo re-propongas sin un argumento nuevo

| Idea | Por qué no |
|---|---|
| Procesos externos como mecanismo principal | No hay frontera de confianza. La puerta queda abierta vía `ProcessModule` |
| WASM / `extism` | Hacen falta PTYs, procesos y sockets: un sandbox con esos agujeros no lo es |
| gpui, Electron | Inestable / contradice Rust |
| `inventory` / `linkme` | La auto-registración pelea con la matriz de ablación |
| `node-pty` | Es de Node; `portable-pty` deja el PTY en el core |
| headroom / token-proxy en Regulus | Son de la cadena personal de Alexander, no del producto |

### 4.4 Decisiones abiertas que siguen vivas

- ✅ **#10 Registro de módulos — cerrada (2026-10-17):** **a mano**, una línea por
  módulo en `crates/regulus-cli/src/modules.rs` (la que escribirá `regulus module
  install`). Sin auto-registro (§4.3).
- **#12 ACP:** protocolo que ~45 agentes ya hablan. Importa al hacer el módulo de
  agentes/comunicación. Investigación en [`old/07-ecosystem.md`](old/07-ecosystem.md).

### 4.5 Las trampas

1. **Construir el framework y nunca las features.** Toda pieza de la base necesita un
   módulo de §2 que la pida.
2. **Absorber hcom antes de la matriz de ablación.**
3. **Una UI bonita y un core que nunca llega.** Por eso el paso 3 va ahora.

---

## 5. Cómo trabajar con Alexander

| Cosa | Cómo |
|---|---|
| Idioma | Español, directo. Código, UI y commits en inglés (decisión #20); los docs en español |
| Formato | Modo ADHD: primero la acción, listas numeradas, máximo 5 ítems, una acción siguiente al final |
| Decisiones | 2-4 opciones con su coste real, la recomendada primero. Explicar en llano, sin jerga |
| Cuando corta un tema | Se acabó. No lo retomes de lado |
| Qué valora | Que le digas cuándo su instinto es correcto y por qué, y cuándo algo está mal. No un sí-señor |
