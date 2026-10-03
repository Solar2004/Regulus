# Handoff — optimizar el harness para que los agentes rindan al máximo

> **2026-10-03** · ❓ **ABIERTO** — línea de investigación, nada decidido.
> Para la próxima sesión, sin memoria de esta. Objetivo de Alexander: que los agentes que
> trabajan aquí (Claude Code, opencode) trabajen de forma óptima y saquen la mejor nota
> posible en benchmarks. La pregunta es **qué mejorar en el harness**, no en el modelo.

---

## 1. Qué existe ya (léelo antes de investigar)

| Qué | Dónde | Estado |
|---|---|---|
| **docguard**: contrasta lo que hizo el agente con las reglas de los docs y lo para si no cumple | `docs/10-docguard.md` (cómo y por qué), `harness/docguard/` | 🔶 construido y probado con Claude Code y opencode, **sin commitear** |
| **Skill global `agent-harness`**: cómo construir un harness, con el algoritmo, los pasos, las trampas y el presupuesto de contexto | `~/.agents/skills/agent-harness/` (symlinks en `~/.claude-sub/skills` y `~/.claude/skills`) | hecha |
| **Base de investigación** ya leída: 25 fuentes, cada una con la decisión de diseño que implica | `~/.agents/skills/agent-harness/references/research.md` | no la repitas; amplíala |

Lo que ya sabemos, comprimido: lo determinista va antes que el LLM; nada de autorrevisión
por prompt; las prohibiciones se aplican antes de la acción; el feedback nombra la cláusula
exacta; las reglas se inyectan justo a tiempo y por ruta; el juez solo veta, cita evidencia
y recibe pocas reglas por llamada; nada duplicado en contexto; tono sereno; los prompts que
solo lee un modelo, en inglés.

## 2. La regla para esta línea: medir antes de optimizar

Principio 3 (Rápido) de `00-principles.md`: hay números, no adjetivos. **Hoy no hay ninguna
medición del efecto de docguard sobre el rendimiento del agente**, solo pruebas puntuales
(2 violaciones sembradas detectadas y 1 cambio limpio sin falsos positivos). Antes de
proponer mejoras hace falta una eval local:

- un **gold set** de cambios con violaciones conocidas y otros limpios → precisión y recall
  del juez;
- **tareas reales del repo** ejecutadas con y sin cada capa → éxito, coste en tokens y tiempo.

Sin esto, cualquier "mejora" es opinión. Es la primera tarea.

## 3. Por dónde seguir investigando

Busca fuentes primarias (arXiv, docs de Anthropic/OpenAI, informes de los primeros puestos
de los leaderboards). Cada hallazgo útil va como fila nueva en `research.md` de la skill,
con su fuente y la decisión que implica.

1. **Qué hacen los mejores harnesses en benchmarks agénticos** (SWE-bench Verified,
   Terminal-Bench): bucles de verificación con tests, cómputo en tiempo de inferencia
   (varios intentos + selección), planificar antes de actuar, subagentes para aislar el
   contexto, compactación. Punto de partida: "An Empirical Study of Harness Design for
   Coding Agents" ([arXiv 2609.20804](https://arxiv.org/html/2609.20804v1)) y "Building AI
   Coding Agents for the Terminal" ([arXiv 2603.05344](https://arxiv.org/html/2603.05344v1)).
2. **Cuándo merece la pena verificar.** Control bayesiano de qué comprobación lanzar y
   cuándo parar ([arXiv 2606.24453](https://arxiv.org/html/2606.24453v1)); explorar teniendo
   en cuenta el coste ([arXiv 2602.16699](https://arxiv.org/html/2602.16699v1)).
3. **Harness que aprende.** Convertir las correcciones de Alexander en reglas ejecutables
   ([arXiv 2606.13174](https://arxiv.org/pdf/2606.13174)), acumular reglas a partir de las
   revisiones ([arXiv 2607.13091](https://arxiv.org/html/2607.13091)), evolucionar el harness
   justo a tiempo ([arXiv 2608.25593](https://arxiv.org/pdf/2608.25593)).
4. **El contexto global de Alexander.** Tiene más de 30 entradas de skills (varias son
   paquetes con muchas dentro), hooks de sesión que inyectan meta-skills enteras y varios
   MCP. La investigación dice que más skills significa peores agentes por elegir la
   equivocada ([arXiv 2605.24050](https://arxiv.org/html/2605.24050v1)). Hay que auditar qué
   se carga en cada sesión y cuánto pesa, y proponer qué podar. **Propón; no borres nada
   sin su aprobación.**
5. **Cuántas herramientas y MCP hay activos, y cómo están descritos**: el efecto del
   número de tools y del solapamiento entre ellas en la selección.

## 3b. Resultados de la sesión 2 (2026-10-03)

Las 8 filas nuevas, con fuente, están al final de `research.md` de la skill. Resumen:

- **El harness pesa tanto como un modelo nuevo.** Con el modelo congelado, cambiar solo el
  harness da +7 a +14 puntos en Terminal-Bench 2.x (StateM, DarwinX, LangChain). Los
  fallos que quedan están en el bucle: perder el estado, no reaplicar lo aprendido, saltarse
  procedimientos y **parar antes de tiempo**. Lo que funcionó: interceptar la salida y
  obligar a verificar, y detectar bucles de edición.
- **Es específico del modelo.** Planificar ayuda a modelos débiles (+11,6) y en los
  fuertes solo abarata (~-30 % de coste); herramientas propias o solo bash cambia de signo
  según el modelo (arXiv 2609.20804). → La eval mide el harness con el modelo con el que
  se trabaja.
- **Cuándo verificar** (arXiv 2606.24453): si la comprobación es barata, siempre; si hay
  un crítico barato y fiable, úsalo de filtro para el caro. → Los tests van **siempre** al
  terminar, y el juez LLM solo se lanza si un filtro barato lo justifica (candidato e).
- **La memoria no hace cumplir las correcciones**: con Mem0 se siguen violando el 57,5 %.
  Si se compilan en comprobaciones previas a terminar, las violaciones bajan del 100 % al
  37,6 % en tareas similares y al 2,0 % en distintas (TRACE). → Respalda el candidato c.
- **Cualquier cambio en el harness necesita una regresión sobre un conjunto reservado**
  antes de adoptarse (Self-Harness: así mejoraron las 9 combinaciones de modelo y benchmark).

### Auditoría del contexto de cada sesión (medida en esta sesión, perfil `~/.claude-sub`)

| Qué se carga antes de la primera palabra | Tamaño | Problema |
|---|---|---|
| Listado de skills: **95 skills** (agent-skills 32, caveman 14, superpowers 14, mattpocock 11, otras 24) | 27,4k caracteres (~7k tokens) | Solapamientos, ver abajo |
| Hooks `SessionStart` de `addyosmani-agent-skills` (9,8k) y `obra-superpowers` (3,3k) que inyectan meta-skills enteras | 13,1k caracteres (~3,3k tokens) | **Se contradicen** («invoca brainstorming antes de cualquier respuesta» frente al flujo de decisión de agent-skills) y usan `EXTREMELY_IMPORTANT` / `YOU MUST`, que hacen que los modelos actuales los activen de más |
| CLAUDE.md ×3 + MEMORY.md | 11,3k caracteres | El de Regulus es sobre todo un mapa y una visión general, y según arXiv 2602.11988 las visiones generales no ayudan. Prioridad baja |
| Prompt de sistema + instrucciones de MCP (chrome, Claude Docs, higgsfield) | ~15k caracteres | Las herramientas ya se cargan diferidas, eso está bien |

Hay **~40k caracteres (~10k tokens) candidatos a podar** en skills y hooks. Habilidades duplicadas (skill shadowing, arXiv 2605.24050):

- **TDD ×4**: `agent-skills:test-driven-development`, `agent-skills:test`, `superpowers:test-driven-development` y `mattpocock-skills:tdd`.
- **Revisión de código ×6**: `agent-skills:code-review-and-quality`, `agent-skills:review`, `superpowers:requesting-code-review`, `mattpocock-skills:code-review`, `caveman-review` y `code-review` (integrada).
- **Depuración ×4**: `agent-skills:debugging-and-error-recovery`, `superpowers:systematic-debugging`, `mattpocock-skills:diagnosing-bugs` e `investigate-first`.
- **Planificación ×3**: `agent-skills:planning-and-task-breakdown`, `superpowers:writing-plans` y `agent-skills:plan`.

**Propuesta, pendiente de Alexander (no se ha tocado nada):** una sola familia por función y
quitar las dos inyecciones de `SessionStart`. Hay que medirlo con la eval antes y después.

## 4. Candidatos de mejora — estado (sesión 3, 2026-10-03)

| # | Mejora | Estado |
|---|---|---|
| b | **Eval** | ✔ `docguard eval`: gold set en `harness/docguard/eval/cases/` (13 casos: 4 `forbid`, 5 semánticos, 4 limpios). Capas deterministas: 13/13. Con el juez (`muse-spark-1.3-contributor-free`): recall 1,00 y precisión 0,75 → 1,00 tras corregir 3 etiquetas incompletas del gold set, que eran violaciones reales (misma ejecución, recalculado). Con 13 casos la cifra es orientativa: añade cada fallo real del juez como caso nuevo. Falta: la eval de **tareas reales con y sin cada capa**, con un grupo reservado |
| a | **Verificar al terminar** | ✔ capa `check` en docguard (no hizo falta un segundo harness): `tsc` del frontend (`crates/regulus-tauri/frontend/`), `py_compile` de `harness/` y `cargo test`, que ya está preparado para cuando exista `crates/`. Probado en un clon: bloquea, desbloquea y avisa del timeout |
| f | **Detector de bucles** | ✔ `loop_edits = 8`, sin medir |
| e | Juez solo si un filtro barato lo justifica | ✔ en parte: si `check` o `forbid` fallan, el juez no se llama |
| g | Podar el contexto global | ✔ hecho en el perfil `~/.claude-sub`, fuera del repo: de 95 a 57 skills, sin las inyecciones de `SessionStart`, ~6k tokens menos por sesión. Se deshace con `~/.claude-sub/skill-prune.py revert`, restaurando `~/.claude-sub/backups/settings.json.pre-skillprune-20261003` y renombrando `hooks.json.off` → `hooks.json` en `addyosmani-agent-skills` y `obra-superpowers` |
| c | **`docguard learn`** (correcciones → reglas) | pendiente. TRACE: del 100 % de violaciones al 2-38 %. Mirar antes `tellonce` |

**Pendiente encontrado con la eval:** la deduplicación del compilador dejó al menos 5
pares de reglas que dicen lo mismo: `02-d15a37`/`04-a386e6` (proyecto actual),
`02-e539c3`/`04-a9fb6b` (`project_id`), `04-b7aedc`/`06-86c93d` (transporte),
`04-a9e7cc`/`06-2160b9` (`hello`) y `02-d860bc`/`06-83967e` (PTY). Engordan las llamadas al
juez; el usuario no ve duplicados porque docguard da un solo aviso por línea. Arreglarlo
exige recompilar, y eso cambia los IDs: hazlo junto con una actualización del gold set.

Arreglado de paso: un pendiente resuelto volviendo al contenido original se daba por no
resuelto y, al repetir ese mismo estado roto, no se volvía a bloquear.

## 5. Pendiente de la sesión anterior

- **Commit** de docguard, la doc y AGENTS.md: no se ha hecho; Alexander no lo ha pedido aún.
- Revisar `harness/docguard/rules.json`: 2 reglas `deny` compiladas están propuestas
  (`"enabled": false`).
- Aprobación de docguard (🔶 en `docs/10-docguard.md`).
- Los `opencode` ya abiertos solo cargan el plugin al reiniciarse.

## 6. Cómo trabajar con Alexander en esto

- Quiere **investigación con fuentes**, no opiniones: cita cada afirmación.
- **WebFetch se inventa cifras al resumir PDFs** (pasó con TRACE). Verifica cada número
  contra el texto: `pdftotext fichero.pdf -`.
- Respuestas en español, directas. Antes de construir, enséñale el diseño y espera su visto
  bueno.
- **Nada de agentes vivos en su repo real**: las pruebas con un agente van en un clon
  desechable (`git clone . /tmp/x`). Trabaja con varios agentes a la vez en el mismo árbol.
- Las skills globales van en `~/.agents/skills/<nombre>/`, con symlinks en
  `~/.claude-sub/skills` y `~/.claude/skills`.
