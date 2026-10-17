# AGENTS.md — Archivo maestro de Regulus

> **Este es el mapa. Léelo completo (~1.7k tokens) antes de abrir cualquier otro
> documento.** Describe qué contiene cada fichero para que no tengas que leerlos todos.
> Actualizado: 2026-10-17 · documentación vigente: ~19k tokens. Lo antiguo está en `docs/old/`.

---

## Qué es Regulus, en tres líneas

Un host de módulos con interfaz visual para construir, conectar y operar sistemas de
agentes de IA. Absorbe código ajeno y lo hace propio **sin perder la capacidad de
quitarlo**. El producto no son las features: es poder añadirlas y quitarlas sin que nada
se rompa.

**Estado:** la base funciona (`regulus serve`, CLI, módulo `hello`, matriz de ablación) y
el canvas carga del core real; la caja Agente corre Claude Code y opencode por ACP, con
proveedor conectado, perfil aislado y secretos. **Lo siguiente son las Skills.** Plan y
orden en [`docs/plan.md`](docs/plan.md).

---

## ⚠️ Los principios mandan sobre todo lo demás

> **"Nuestro producto tiene que ser como el agua."** — Alexander

**Flexible · Modular · Rápido · Personalizable · Infraestructura.**

Cada principio tiene una prueba concreta que se puede fallar. **Lee
[`docs/00-principles.md`](docs/00-principles.md) (~1.2k tokens) antes de escribir código o
tomar una decisión de diseño.** Si una decisión de cualquier otro documento viola un
principio, el principio gana.

Las cinco preguntas que se contestan antes de aprobar cualquier cambio:

1. ¿Hay algún literal que debería ser configuración? *(Flexible)*
2. ¿Sigue pasando la matriz de ablación? *(Modular)*
3. ¿Se midió, y entra en presupuesto? *(Rápido)*
4. ¿Se puede cambiar sin recompilar? *(Personalizable)*
5. ¿Existe el comando de CLI equivalente? *(Infraestructura)*

Y el guardarraíl, que está en el mismo documento: **flexibilidad es no cerrar puertas, no
construir todas las habitaciones hoy.**

---

## Mapa de documentos

| Fichero | Tokens | Qué contiene |
|---|---|---|
| **`docs/plan.md`** | ~1.8k | **Si retomas en frío, empieza aquí.** Qué es base y qué es módulo, orden de trabajo, la pregunta abierta, lo rescatado de los docs antiguos (doctrina Predator, descartados, trampas) y cómo trabajar con Alexander. |
| **`docs/concept.md`** | ~1k | Qué es Regulus en llano: piezas (módulo, nodo, cable, workflow, perfil, ejecución) y las dos vistas. |
| **`docs/00-principles.md`** | ~1.2k | **Manda sobre todos los demás.** "Como el agua", los 5 principios con su prueba, presupuestos de rendimiento, el guardarraíl. |
| **`docs/11-composition.md`** | ~2.6k | ✅ **El modelo**: base sin funciones, nodos, cables "usa" y "luego", workflows, perfiles, ejecuciones, secretos, instalar módulos. Respuestas de Alexander (§12). |
| **`docs/12-agents.md`** | ~3.4k | ✅ **Agentes**: la caja Agente y sus motores (Claude Code, opencode…) como módulos, perfil seleccionable, canal ACP, el contrato `agent.engine@1`, **proveedores** (`llm.endpoint@1`), perfil aislado y lo construido. |
| **`docs/02-architecture.md`** | ~2.5k | Los dos límites (socket / trait Rust), por qué módulos in-process, forma del workspace, **las abstracciones del core** (`Node`, `Port`, `Wire`, `Workflow`, `Profile`, `Execution`), bus y store. |
| **`docs/03-modules.md`** | ~2.2k | El contrato: trait `Module`, `ModuleCtx`, manifest con nodos/puertos/campos, workflow interno, estados, pánicos, **matriz de ablación**. |
| **`docs/04-client.md`** | ~0.9k | El cliente: el mock es la spec, Canvas + Operar, estados de primer nivel, `hello`, multi-proyecto, idioma y tema. |
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
| **Retomar en frío / no sé qué toca** | `00-principles.md` → `plan.md` | ~3k |
| **Entender el proyecto** | `concept.md` → `11-composition.md` | ~3.6k |
| **Tocar el core** | `02-architecture.md` → `03-modules.md` → `06-stack.md` §2.1 | ~5k |
| **Escribir un módulo** | `plan.md` §4.1 → `03-modules.md` | ~2.5k |
| **Tocar agentes, motores o proveedores** | `12-agents.md` → `03-modules.md` | ~4.8k |
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
| 1 | **Skills** (se enchufan a la caja Agente; se colocan en el perfil, `12-agents.md` §3) | nada |
| 2 | **Comunicación** (hcom) | el anterior |

---

## Contexto del entorno

- Alexander trabaja en **Arch Linux**, kitty + tmux. Proyectos en `~/Projectos/`.
- **Responde en español, directo y sin rodeos.**
- `~/Projectos/hcom` es su fork propio (`Solar2004/hcom`) — modificable, pero **no
  todavía**.
- El repo es git (`master`).
