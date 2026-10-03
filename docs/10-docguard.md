# 10 — docguard: que los agentes sigan la documentación

> **v0.2** · 2026-10-03 · 🔶 **PROPUESTO** — construido y probado, pendiente de aprobación
> de Alexander.
>
> Vale para todo agente que trabaje en este repo. Si eres un agente: no tienes que hacer
> nada. docguard trabaja solo y solo te habla cuando algo no cumple.

---

## 1. Qué hace, en una frase

Al terminar un turno, mira **lo que hizo el agente**, lo contrasta **con las reglas de la
documentación que aplican a esos ficheros** y, si algo no cumple, **le impide parar** hasta
que lo corrija o lo justifique.

## 2. Las capas, de barata a cara

| Capa | Cuándo | Qué | Coste |
|---|---|---|---|
| `deny` | Antes de cada comando o edición | Bloquea acciones prohibidas (`--no-verify`, editar `~/Projectos/hcom`) o pide confirmación (push, tags) | 0 tokens |
| Contexto justo a tiempo | La primera vez que el agente toca una ruta | Le inyecta **solo** las reglas de esa ruta, una vez por sesión | ~100-400 tokens |
| Detector de bucles | Cada edición | Al llegar a `loop_edits` ediciones del mismo fichero, avisa una vez de que replantee | ~60 tokens, una vez |
| `check` | Al terminar | Corre los comandos de verificación (`check = <globs> :: <comando>`: `tsc`, `cargo test`, `py_compile`) cuyas rutas tocó el agente; si fallan, le da la salida y no le deja parar | 0 tokens, segundos |
| `forbid` | Al terminar, si `check` pasa | Regex sobre las líneas añadidas | 0 tokens |
| `decide` | Al presentar opciones (`AskUserQuestion`, `ExitPlanMode`, `question`) y al terminar, si `check` pasa | Si el agente elige entre caminos ("opción A/B", "en vez de", "elegí"…) sin nombrar ningún principio, se le para **una vez** con el checklist de `00-principles.md` (`prompts/decide.md`). Marcadores en `decide_markers` / `principle_markers` | 0 tokens |
| `semantic` | Al terminar, si `check` y `forbid` pasan y alguna regla aplica a lo tocado | Juez headless (opencode) con el diff y **solo** esas reglas; segunda pasada que filtra nitpicks | ~5-15k tokens, gratis con Zen free |

Sin cambios nuevos no se hace nada (0,1 s). Un estado ya revisado no se vuelve a pagar.

## 3. Por qué así (la investigación, comprimida)

- **No pedirle al agente que se autorrevise.** En ContextCov, la "reflexión" bajó el
  cumplimiento al 50,3 %, por debajo de no hacer nada (67 %); compilar las reglas a
  comprobaciones lo subió al 88,3 %, con 3,4× menos coste.
  [arXiv 2603.00822](https://arxiv.org/html/2603.00822v2)
- **Compilar los docs una vez, no leerlos en cada turno.** Las reglas viven en
  `rules.json` con su ruta de aplicación; solo se recompila la sección que cambia.
- **Determinista antes que LLM.** Un verificador en código coincide con los humanos en
  113/120 casos; un juez LLM, en 95/120. El juez solo veta, nunca aprueba.
- **El juez ve poco y tiene que citar.** Solo los hunks cambiados y las reglas cuyo scope
  los toca (más contexto empeora: arXiv 2606.15828). Cada hallazgo tiene que citar
  literalmente una línea añadida o se descarta solo. Checklist de obligaciones primero
  (arXiv 2508.12358) y filtro de nitpicks después (arXiv 2505.17928).
- **Un mensaje que nombra la cláusula exacta** sube el cumplimiento al 77-100 %…
- **…salvo en las prohibiciones**, que no se arreglan a posteriori (0 %): por eso van en
  `deny`, antes de la acción. [arXiv 2607.26819](https://arxiv.org/html/2607.26819v1)
- **Reglas en el momento de decidir**, no un prompt gigante al principio: el cumplimiento
  se degrada a lo largo de la sesión. [arXiv 2603.05344](https://arxiv.org/html/2603.05344v1)
- **Verificar antes de parar es la palanca más grande.** Con el modelo fijo, cambiar solo
  el harness da +7-14 puntos en Terminal-Bench; los fallos que quedan son parar antes de
  tiempo sin comprobar y dar vueltas sobre lo mismo
  ([StateM](https://arxiv.org/abs/2608.15089), [LangChain](https://www.langchain.com/blog/improving-deep-agents-with-harness-engineering)).
  Si comprobar es barato, se comprueba siempre; un filtro barato decide si se paga el
  caro ([arXiv 2606.24453](https://arxiv.org/html/2606.24453v1)). Por eso `check` va
  primero y, si falla, no se llama al juez.
- **Nunca en subagentes** y con tope de rondas (`max_rounds`): sin eso, bucles.
- **El propio harness es contexto y se presupuesta.** Los ficheros de contexto no suben el
  éxito y cuestan +20 % ([arXiv 2602.11988](https://arxiv.org/abs/2602.11988)); la
  precisión cae al acumular instrucciones ([arXiv 2507.11538](https://arxiv.org/abs/2507.11538)).
  Por eso: no se re-inyecta nada de `always_loaded` (AGENTS.md ya está en contexto), las
  reglas más específicas van primero, el juez recibe tandas de `judge_rules_per_call`, el
  compilador recibe las reglas manuales para no duplicarlas, y los prompts que solo lee un
  modelo van en inglés en tono sereno, sin mayúsculas de énfasis (las reglas citadas
  siguen en español, como los docs).

## 4. Ficheros

```
harness/docguard/
├─ docguard.py        núcleo: CLI + adaptadores (Python 3 sin dependencias)
├─ docguard.conf      toda la configuración (modelo, rondas, rutas ignoradas…)
├─ rules.json         reglas: manuales + compiladas desde los docs
├─ prompts/           juez, verificación, compilación, deduplicación
└─ eval/cases/        gold set: cambios con la violación esperada, y cambios limpios
.claude/settings.json            hooks de Claude Code (SessionStart, PreToolUse, Stop)
.opencode/plugins/docguard.ts    plugin de opencode (session.idle, tool.execute.*)
```

Quitarlo entero: borra esas tres rutas. Nada más depende de ellas.

## 5. Uso desde la CLI

```bash
harness/docguard/docguard.py check            # revisa lo que hay sin commitear (exit 1 = fallos)
harness/docguard/docguard.py rules ui/src/x   # qué reglas aplican a una ruta
harness/docguard/docguard.py compile          # recompila las secciones de docs que cambiaron
harness/docguard/docguard.py reset            # olvida el estado de las sesiones
harness/docguard/docguard.py eval             # precisión y recall contra el gold set (con juez)
harness/docguard/docguard.py eval --no-judge  # solo las capas deterministas, en 0,1 s
```

**Si cambias el juez, sus prompts o las reglas**, ejecuta `eval` antes y después: un
cambio que no mejora la cifra no entra. Cada caso de `eval/cases/` es un JSON
`{file, before, after, expect, note}`; `expect: []` es un cambio limpio y mide los falsos
positivos. Cuando el juez falle en algo real, añade ese caso. El informe queda en
`.git/docguard/last-eval.json`.

**Si cambias un documento**, ejecuta `compile` y revisa el diff de `rules.json` como
cualquier otro cambio. Las reglas `deny` compiladas nacen con `"enabled": false`:
bloquean sin juicio, así que las activa un humano.

## 6. Decisiones y límites conocidos

- **Juez: `opencode/muse-spark-1.3-contributor-free` con el agente `plan`.** El tier
  gratuito de Zen rechaza agentes propios o herramientas recortadas ("free tier can only
  be used from within OpenCode"). `plan` es de solo lectura por su prompt; por eso el juez
  corre además en un directorio temporal vacío.
- **Atribución por ficheros tocados** (`attribution = touched`). Con varios agentes en el
  mismo árbol, git no sabe quién cambió qué. Coste: lo que un agente cambie por bash no se
  le revisa. `attribution = worktree` revisa todo lo que cambió desde que empezó la sesión.
- **Diffs grandes se reparten, no se cortan.** opencode trunca los adjuntos grandes; con
  el formato de respuesta al final del prompt, el juez se quedaba sin él y no devolvía JSON
  (fallo real, 2026-10-03). Ahora el formato va antes del diff, el diff se reparte por
  ficheros en paquetes de `max_diff_bytes` y hay un tope de `judge_max_calls`; si se
  alcanza, se avisa de que la revisión fue parcial. Si el juez falla, su respuesta cruda
  queda en `.git/docguard/last-judge-output.txt`.
- **Un fallo del harness nunca bloquea.** Juez caído, timeout o JSON roto, o un `check`
  que se pasa de `check_timeout`: el agente para y tú recibes el aviso.
- **`check` corre en tu árbol de trabajo**, no en un sandbox: solo comandos de
  verificación, nunca algo que escriba fuera de `target/` o `node_modules/`.
- **`decide` mira palabras, no intenciones.** Es regex: una elección redactada sin
  marcadores se le escapa, y basta nombrar un principio para pasar. Su trabajo es que el
  agente *se pare a pensar* en los principios en el momento de decidir (arXiv 2603.05344),
  no auditar si la elección fue buena; eso sigue siendo cosa tuya. Para no entrar en
  bucle, cada texto se para una sola vez. Los marcadores están sin medir.
  Probado con agentes vivos en un clon (2026-10-03): en opencode y en Claude Code el
  agente elige sin principios → se le para → relee `00-principles.md` y rehace la
  elección. En Claude Code el `matcher` de `PreToolUse` tiene que incluir
  `AskUserQuestion|ExitPlanMode`. El nombre `question` de opencode está sin verificar:
  en `opencode run` esa herramienta no aparece.
- **En `opencode run`** (headless) la ronda de corrección no llega a ejecutarse: el proceso
  sale al quedar inactivo. En la TUI y en `opencode serve` sí.
