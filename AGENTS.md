# AGENTS.md — Archivo maestro de Regulus

> **Este es el mapa. Léelo completo (~1.7k tokens) antes de abrir cualquier otro
> documento.** Describe qué contiene cada fichero para que no tengas que leerlos todos.
> Actualizado: 2026-10-02 · total de la documentación: ~21k tokens.

---

## Qué es Regulus, en tres líneas

Un host de módulos con interfaz visual para construir, conectar y operar sistemas de
agentes de IA. Absorbe código ajeno y lo hace propio **sin perder la capacidad de
quitarlo**. El producto no son las features: es poder añadirlas y quitarlas sin que nada
se rompa.

**Estado:** diseño. No hay código de aplicación todavía. Branding sí
(`assets/branding/`).

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

| Fichero | Tokens | Qué contiene | Decisiones que viven aquí |
|---|---|---|---|
| **`docs/HANDOFF.md`** | ~2.4k | **Si retomas en frío, empieza aquí.** TODO actual en orden con estimaciones. El **razonamiento** detrás de cada decisión grande. Lo descartado y por qué (no lo re-propongas). Cómo trabajar con Alexander. Estado del repo. Las 3 trampas del proyecto. | — |
| **`docs/00-principles.md`** | ~1.2k | **Manda sobre todos los demás.** La regla de oro ("como el agua"). Los 5 principios, cada uno con una prueba que se puede fallar. Presupuestos de rendimiento numéricos. El guardarraíl contra el alcance infinito. | Los 5 principios |
| **`docs/01-vision.md`** | ~2.8k | Por qué existe Regulus. La **doctrina Predator** (qué significa "módulo"). Qué NO es. Los 5 casos de uso canónicos y las 5 capacidades que el core necesita. Requisitos sacados del workflow real. Nombre y marca. Glosario. | Visión, no-objetivos, alcance |
| **`docs/02-architecture.md`** | ~2.2k | **Los dos límites** (clientes↔core por socket; core↔módulos por trait Rust). Por qué los módulos son in-process. Forma del workspace. Abstracciones del core (`Agent`, `Isolation`, `Project`). Bus y store. | #1 repo, #2 módulo in-process, #3 servidor día 1 |
| **`docs/03-modules.md`** | ~1.8k | El contrato: trait `Module`, `ModuleCtx`, capacidades vs nombres, manifest, estados de módulo, contención de pánicos. **La matriz de ablación.** Cómo escribir un módulo. | El contrato completo |
| **`docs/04-client.md`** | ~2.4k | Por qué el cliente va primero (mock server = spec del core). Las **dos vistas** con mockups ASCII. **Los 8 métodos RPC** derivados de ellas. Estados vacíos. Handshake. Regulus como ACP Client. | #4 dos vistas, #9 cliente primero |
| **`docs/05-roadmap.md`** | ~2.6k | Fases A→F de v1 con definición de terminado y estimaciones. **Tabla de decisiones cerradas y abiertas.** Riesgos. **Los 6 briefs** para agentes. | Índice de todas las decisiones |
| **`docs/06-stack.md`** | ~2.2k | Dependencias concretas con versiones verificadas. Reparto de la terminal (xterm.js arriba, `portable-pty` en el core). Transporte. **Sistema de versionado de tres ejes.** | #7 Tauri, #8 transporte, #13 versionado, #14 terminal |
| **`docs/07-ecosystem.md`** | ~2.2k | **ACP**: ~45 agentes ya lo hablan. `rmcp` para MCP. Candidatos de registro de módulos. Implementaciones de referencia. **Lo que NO se va a usar y por qué.** | #12 ACP (abierta) |
| **`docs/08-design-system.md`** | ~1.8k | UI kit recomendado, lenguaje visual Regulus Midnight, paleta y tokens en `assets/design/`, integración futura y muestra HTML. | Kit propuesto, aún no instalado |
| **`docs/09-git.md`** | ~1.8k | **Cómo se commitea y se empuja aquí.** Formato de mensaje, tipos y scopes, qué entra en un commit, política de ramas y push, releases. **Las 7 reglas para agentes.** Lo aplican los hooks de `.githooks/`; la configuración viva está en `.gitconventions`. | #19 harness de git |
| **`docs/research.md`** | ~1.3k | Datos medidos, no opiniones. Orca (es Electron/TS, no Rust). hcom (es Rust, binario sin `[lib]`). El workflow real de Alexander con cifras. | — |
| **`docs/ideas/boxes.md`** | ~1.4k | El sistema de cajas: **sin definir**. Tres lecturas posibles, 5 preguntas sin responder. Bloquea `Isolation`. | #11 cajas (abierta) |
| **`README.md`** | ~1.0k | Cara humana: branding, assets, índice de lectura. | — |

---

## Rutas de lectura según tu tarea

| Tu tarea | Lee, en este orden | Coste |
|---|---|---|
| **Retomar en frío / no sé qué toca** | `00-principles.md` → `HANDOFF.md` | ~3.6k |
| **Entender el proyecto** | `01-vision.md` | ~2.8k |
| **Escribir un módulo** | `01` §2 (doctrina) → `03-modules.md` completo | ~2.5k |
| **Tocar el core** | `02-architecture.md` → `03-modules.md` → `06-stack.md` §2.1 | ~4.5k |
| **Diseñar o implementar UI** | `04-client.md` → `06-stack.md` §1-2 → `08-design-system.md` | ~6k |
| **Decidir algo / saber qué toca** | `05-roadmap.md` | ~2.6k |
| **Elegir una dependencia** | `07-ecosystem.md` §5 (lo descartado) → `06-stack.md` | ~1.5k |
| **Verificar un dato sobre Orca o hcom** | `research.md` | ~1.3k |
| **Commitear, ramificar o empujar** | `09-git.md` §1-5 | ~1.2k |
| **Trabajar en un brief** | `05-roadmap.md` §5 → lo que tu brief indique | variable |

---

## Reglas duras

### 1. La convención de marcas

| Marca | Qué hacer |
|---|---|
| ✅ **DECIDIDO** | Constrúyelo. No lo cuestiones ni lo "mejores". |
| 🔶 **PROPUESTO** | Hay recomendación con razonamiento. Necesita aprobación de Alexander. |
| ❓ **ABIERTO** | **No lo inventes.** Pregunta. |

### 2. No empieces nada bloqueado

- **Implementar `Isolation`** → espera la decisión #11 (cajas)
- **Absorber hcom o tocar `~/Projectos/hcom`** → espera v1 completo y la matriz de ablación
- **Paneles de ACP (plan, tool calls, permisos)** → esperan a v2

**La UI ya NO está bloqueada.** Decisiones #7, #15, #16, #17 y #18 cerradas.

### 3. Los dos invariantes que no se negocian

1. **Quitar cualquier módulo deja el resto funcionando.** Verificable con la matriz de
   ablación (`03-modules.md` §4). No es una intención.
2. **El core nunca conoce un módulo por su nombre**, solo por capacidad. Si escribes
   `if module.id == "agents"` en el core, está mal.

### 4. La regla de alcance

Toda capacidad del core debe estar justificada por un caso de uso de `01-vision.md` §4.
**Si ningún UC la pide, no se construye.** El riesgo nº1 del proyecto es construir el
framework y no llegar nunca a las features.

### 5. Un solo sitio por dato

Las versiones de dependencias viven **solo** en `06-stack.md`. Los datos medidos viven
**solo** en `research.md`. Si duplicas, se desincroniza. Si encuentras una duplicación,
bórrala y deja un enlace.

### 6. Cómo se commitea

El repo tiene hooks: un mensaje mal formado **no entra**. Lo mínimo que necesitas saber
antes de tu primer commit — el resto está en [`docs/09-git.md`](docs/09-git.md):

```bash
scripts/install-hooks.sh --check    # ¿hooks activos en este clon? si no: scripts/install-hooks.sh
```

```
tipo(scope): asunto en imperativo, minúscula, sin punto, ≤72 caracteres

El por qué. El qué está en el diff.

Refs: docs/05-roadmap.md §2
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
| 1 | **Fase A**: mock server con los 8 métodos de `04-client.md` §3 + `hello` | nada |
| 2 | **Fase B**: UI vista OPERAR contra el mock | nada |
| 3 | **BRIEF-2**: diseño de alta fidelidad de las dos vistas | nada |
| 4 | **BRIEF-6**: spike de ACP con un agente real (medio día) | nada |
| 5 | **BRIEF-5**: entrevistar a Alexander sobre las cajas | Alexander |

---

## Contexto del entorno

- Alexander trabaja en **Arch Linux**, kitty + tmux. Proyectos en `~/Projectos/`.
- **Responde en español, directo y sin rodeos.**
- `~/Projectos/hcom` es su fork propio (`Solar2004/hcom`) — modificable, pero **no
  todavía**.
- El repo es git (`master`), sin commits aún.
