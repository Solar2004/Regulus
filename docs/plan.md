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
| **Proveedores** | Conectar OpenAI, Anthropic, modelos locales, suscripciones; pool, plan B, router | API keys |
| **Agente** | Un agente propio de Regulus que usa un proveedor | — |
| **Code** | Workspace, herramientas de programación e instrucciones del repositorio; harness compuesto con los módulos de agentes | agente de programación en TUI y app |
| **Evidencias y datos** | Fuentes versionadas, referencias comprobables, datasets y comparación de snapshots | piezas genéricas para investigación, evaluación y seguimiento |
| **Modelos** | Elegir o rotar modelos: hoy son las ranuras de la caja Agente y las envolturas de Proveedores (`12-agents.md` §6) | — |

Más adelante, también como módulos: roles, familias, MCP aislado, memoria.

✅ **DECIDIDO (2026-10-17) — cada módulo es una caja líquida** (`00-principles.md`):
forma fija, motor intercambiable, ranuras por propósito, envolturas en los cables, se abre
y se guarda como plantilla.

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
| 4b | Proveedores, perfil aislado y secretos (`12-agents.md` §6) | — | ✅ hecho (2026-10-17) |
| 4c | Proveedores como caja líquida: plantillas, ranuras, suscripción (`12-agents.md` §7); la pasarela, aplazada | — | ✅ base (2026-10-17) |
| 4d | Skills (`12-agents.md` §3) y Comunicación, base (`11-composition.md` §3) | — | ✅ base (2026-10-18) |
| 4e | Comunicación como caja y módulo Herramientas, 5 pasos (`11-composition.md` §3.0) | — | en curso |
| 4f | **Bloques** (`13-blocks.md` §7): core (cable "luego" con dato, store, workflow como caja, `Call`), ranura `hooks`, módulos de lógica y entorno, plantillas | ~5 sem | ✅ decidido (2026-11-07); core, módulos y 7 de 8 plantillas hechos (2026-11-08); research cerrada e2e con Claude y los arreglos del core que las plantillas anotaron hechos (2026-11-08, `f150fbd`); pxl como plantilla, corrida con Claude (2026-11-08, `e9f9109`): las ocho plantillas hechas |
| 5 | Canvas editable: crear nodos, tirar cables, guardar (`04-client.md` §9) | ~1 sem | ✅ hecho (2026-11-08, medio día; antes el core ganó `module.manifest`, `profile.save` y `profile.delete`) |
| 6 | Vista Operar con ejecuciones reales, instalar plantillas (`04-client.md` §8) | ~1 sem | ✅ hecho (2026-11-08, medio día) |
| 7 | **App completa**: Ajustes, TUI que edita como la app y `[client]` de `config.toml` para app y TUI (`04-client.md` §10, §7, §11); proyectos con `regulus.toml` (`11-composition.md` §13) | — | ✅ hecho (2026-11-08, un día); `ProcessModule` para `[[modules.local]]` hecho (2026-11-08, `94e5aa3`); release + `regulus update` + Ajustes → Updates hechos (2026-11-08, `52d6bfe`); `blocked` con su botón de confiar en Ajustes y `release.yml` que publica en GitHub Releases (2026-11-08) |
| 8 | **Harness líquido** (contratos C1-C10): "usa" cruza cajas, hijas vivas y `execution.tree`, `template.save`; eventos, permisos e historial del agente; conversación en app, TUI y `regulus talk`; `flow.tool` · `flow.batch` · `delegation`; `documents` · `inference`; motor nativo (`12-agents.md` §5, §8; `13-blocks.md` §3, §5; `04-client.md` §7-8) | 1 día | ✅ hecho (2026-11-09, cinco ramas en paralelo); falta correrlo con agentes y modelos reales |
| 9 | **Regulus Code**: módulo conectable de programación, plantilla de harness editable, TUI y vista Code en Tauri, con mascota compartida (`14-code.md`) | — | construido (2026-11-10); módulo, plantilla, TUI y vista visual integrados; queda pulir con uso real |
| 10 | **Investigación componible y MCP de control**: evidencias y datos como módulos; investigación con citas, laboratorio RAG y seguimiento como plantillas; cliente MCP sobre el RPC existente | — | ✅ hecho (2026-11-10): `evidence` y `data` (`15-evidence.md`, `16-experiments.md`), plantillas `research-evidence` · `rag-lab` · `topic-watch`, `regulus mcp` con `--read-only` y `--deny-module` (`17-control-mcp.md`); sin ampliar el core. Después, el mismo día: el core lee el store de un proyecto sin ejecución viva (`ModuleCtx::project_store`, `02-architecture.md` §6) y `documents` poda con `prune`. Falta correrlo con modelos y clientes MCP reales |
| 11 | **Nodos propios**, primer incremento: SDK de Python sobre el protocolo de módulos locales, `regulus node init`, campos `code` y `multiline` en el inspector (`18-custom-nodes.md`) | — | construido (2026-11-10, sin core nuevo ni API de `ModuleCtx`); probado contra un core real y en un navegador real, revisado y con sus defectos arreglados; falta usarlo en un proyecto real. El segundo incremento es la fila 12; lo que ninguno construyó (§7 y §9 de `18`) es 🔶, sin aprobar |
| 12 | **Nodos propios**, segundo incremento (Alexander autorizó cerrar el editor, el parser, recargar y las variantes): editor CodeMirror y borrador local, editar y comprobar sin ejecutar la fuente de un módulo local, recargarlo sin reiniciar el core, puertos que dependen de la config y sincronizar los viejos (protocolo 1.17); Rust local con `regulus node init --lang rust`, un ayudante de un fichero (`sdk/rust/`) y `node build`; las 17 plantillas en el binario; puerta de CI en los tags (`18-custom-nodes.md` §2, §4, §6; `04-client.md` §13) | — | construido en el árbol de trabajo (2026-11-11), probado contra un core real y en un navegador real; **candidato 0.2.1, sin publicar** (versión, tag y firma son de Alexander); el flujo de CI de los tags está escrito y no se ha ejecutado en GitHub; teclear **no** cumple los 16 ms en el p90 (`18` §4.3). **Sigue 🔶 y sin construir:** `$T` y mínimo/máximo de puertos, depurador y valores por puerto (R1), R2-R4, nodo Script (F5), más `ModuleCtx` por stdio (F6), MCP de control para estos métodos, abrir en editor externo, stderr del módulo en Ajustes |

**Prueba de la base (paso 3):** la UI apunta al core real con `VITE_RPC_URL` y funciona
sin cambiar una línea.

**Hcom no se absorbe antes de que exista la matriz de ablación** (paso 4). Absorber un
módulo grande sin ella es cómo se pierde la modularidad sin enterarse.

**Falta especificar antes de que haga falta:** qué pasa al reconectar tras caerse el
WebSocket. El formato de `regulus.toml` está en `11-composition.md` §13 y los atajos de
teclado en `04-client.md` §11 (`[client.keys]`).

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
| headroom / token-proxy en Regulus | Son de la cadena personal de Alexander, no del producto. La **pasarela** de `12-agents.md` §6.4 es otra cosa: genérica y del producto (Alexander, 2026-10-17) |

### 4.4 Decisiones abiertas que siguen vivas

- ✅ **#10 Registro de módulos — cerrada (2026-10-17):** **a mano**, una línea por
  módulo en `crates/regulus-cli/src/modules.rs`. Sin auto-registro (§4.3). ⏳ Lo que sigue
  **aprobado y no construido** es el comando que la escribiría y recompilaría (`regulus
  module install`, `11-composition.md` §10 opción A): hoy no existe y no se sustituye por un
  gestor de paquetes. Para añadir un módulo sin tocar el binario está el módulo local por
  stdio (`18-custom-nodes.md` §6).
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
