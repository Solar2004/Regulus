# 09 — Git: commits, ramas, push y releases

> **v0.1** · 2026-10-02 · ✅ **DECIDIDO**
> Las reglas que aplica el harness de `.githooks/`. **Vale para todo agente** que escriba
> en este repo: Claude Code, opencode, Codex, Cursor o un humano.
> La configuración viva está en [`.gitconventions`](../.gitconventions); este documento la
> explica. Si los dos se contradicen, manda `.gitconventions` (es lo que se ejecuta).

---

## 0. Antes de tu primer commit

```bash
scripts/install-hooks.sh          # una vez por clon
scripts/install-hooks.sh --check  # ¿está activo?
```

Sin esto los hooks **no corren** (git no versiona `.git/hooks`; el instalador apunta
`core.hooksPath` a `.githooks/`). Si eres un agente y vas a commitear en un clon recién
hecho, corre `--check` primero.

---

## 1. Anatomía de un commit

```
tipo(scope)!: asunto en imperativo, minúscula, sin punto final
                                      ← línea en blanco obligatoria
Por qué, no qué. El qué está en el diff. Qué decisión se tomó, qué
alternativa se descartó, qué documento lo manda.
                                      ← línea en blanco
Refs: docs/05-roadmap.md §2
BREAKING CHANGE: qué rompe y cómo se migra
Co-Authored-By: Nombre del agente <email>
```

| Parte | Regla | Lo aplica |
|---|---|---|
| tipo | de la lista de `.gitconventions`, minúscula | hook, bloquea |
| scope | opcional, pero si está **debe** estar en la lista | hook, bloquea |
| `!` | cambio incompatible; exige footer `BREAKING CHANGE:` | hook, bloquea |
| asunto | 10–72 caracteres, minúscula inicial, sin punto final, ≥2 palabras | hook, bloquea |
| asunto | imperativo presente: *añade*, no *añadido* | hook, avisa |
| cuerpo | línea en blanco antes; líneas ≤100 caracteres | hook, bloquea |
| idioma | inglés en asunto y cuerpo (decisión #20); los docs siguen en español | hook, avisa |

**Un asunto bueno completa la frase “al aplicar este commit, Regulus…”.**

```
✅ feat(core): add protocol version negotiation in hello
✅ docs(design): close decision #17 with the Midnight palette
✅ fix(gui): avoid full repaint on project switch
✅ refactor(modules)!: split ModuleCtx from the registry
   BREAKING CHANGE: modules receive ctx as a parameter, not as a field.

❌ update docs                  (sin tipo, no dice nada)
❌ feat: assorted changes       (asunto vacío de contenido)
❌ feat(ui): …                  (scope inexistente: es `gui`)
❌ feat(core): Add the bus.     (mayúscula y punto final)
❌ fix: fixed the handshake     (participio, no imperativo)
```

---

## 2. Tipos y scopes

| Tipo | Cuándo |
|---|---|
| `feat` | capacidad nueva visible desde fuera |
| `fix` | corrección de comportamiento |
| `refactor` | cambia la forma, no el comportamiento |
| `perf` | mejora medida contra un presupuesto de [`00-principles.md` §3](00-principles.md) |
| `test` | tests, incluida la matriz de ablación |
| `docs` | documentación, incluidos `docs/` y `AGENTS.md` |
| `build` `ci` `chore` | tooling, dependencias, configuración, hooks |
| `revert` `style` | deshacer; formato sin cambio de lógica |

Los scopes vivos están en `.gitconventions` (`core proto cli gui modules store bus
terminal acp mcp docs assets design repo ci deps hooks`). **Un scope nuevo se añade a
`.gitconventions` en el mismo commit que lo estrena**, nunca por separado.

---

## 3. Qué entra en un commit

| Regla | Por qué |
|---|---|
| **Una cosa por commit.** | Si el asunto necesita un “y”, son dos commits. |
| **~100 líneas, 300 como techo razonable.** | Más de 1000 se parte. |
| **Formato y comportamiento no viajan juntos.** | Un diff de 400 líneas de `rustfmt` esconde el cambio real. |
| **Refactor y feature, separados.** | Para poder revertir uno sin perder el otro. |
| **Nada de ficheros que no entiendes.** | `git add -A` a ciegas es cómo se cuela un secreto. Revisa `git diff --staged`. |
| **Decisión de diseño → cítala.** | `Refs: docs/05-roadmap.md §2`. El historial es documentación. |

Mientras el repo sea solo documentación, el commit típico es
`docs(<área>): …` y la regla de atomicidad se lee como **un documento o una decisión por
commit**.

---

## 4. Ramas y push

| Qué | Política hoy | Dónde se cambia |
|---|---|---|
| Commit directo en `master` | **avisa**, no bloquea | `on_protected_commit` |
| Push directo a `master` | **avisa**, no bloquea | `on_protected_push` |
| Reescribir historia publicada de `master` | **bloquea** | `on_protected_rewrite` |
| Nombre de rama fuera de patrón | avisa | `on_bad_branch_name` |
| `fixup!` / `squash!` / `WIP` en el push | **bloquea** | `forbidden_push_prefixes` |

Patrón de rama: `tipo/descripcion-corta` en minúsculas — `feat/mock-server`,
`fix/handshake-proto`, `spike/acp-gemini`. Ramas cortas: se integran en 1–3 días. Una rama
que vive una semana ya no es una rama, es un fork.

Rebase para limpiar **lo que sigue siendo local**. En cuanto está empujado, se arregla con
commits nuevos, no reescribiendo. El bloqueo de rewrite se salta a propósito y dejando
rastro: `REGULUS_ALLOW_REWRITE=1 git push --force-with-lease`.

---

## 5. Reglas para agentes

Esto es lo que se te pide **explícitamente** si no eres humano:

1. **No uses `--no-verify`.** Si un hook falla, arregla la causa. Si crees que la regla
   está mal, dilo y propón el cambio en `.gitconventions` — no la esquives. `pre-push`
   revalida igualmente todo el rango, así que saltarse `commit-msg` solo retrasa el error.
2. **No commitees nada que no te hayan pedido tocar.** Ni “limpieza” de paso, ni
   reformateos, ni dependencias actualizadas de propina.
3. **Firma lo que escribes.** `Co-Authored-By:` con tu identidad real. No inventes la
   coautoría de un humano que no ha escrito el código.
4. **No empujes a un remoto sin que te lo hayan pedido.** Commitear es reversible; empujar
   es público. Tampoco crees tags ni releases por iniciativa propia.
5. **Un commit por incremento verificado**, no uno gigante al final de la sesión.
6. **Si el hook avisa (⚠) y no bloquea, el aviso no es opcional**: arréglalo o explica en
   la respuesta por qué lo dejas así.
7. **Resumen al terminar**: qué ficheros tocaste, qué dejaste sin tocar a propósito, qué
   duda queda abierta.

---

## 6. Releases

Los tres ejes de versión y por qué solo el protocolo es crítico están en
[`06-stack.md` §5](06-stack.md) — no se repiten aquí (regla 5 de
[`AGENTS.md`](../AGENTS.md): un solo sitio por dato). Lo que añade el harness:

- **El tag es la verdad.** `git tag -a v0.2.0 -m "Release 0.2.0"`. La versión del artefacto
  se deriva del tag; nunca se edita a mano en sitios distintos.
- **La entrada de [`CHANGELOG.md`](../CHANGELOG.md) se escribe en el mismo commit que el
  cambio**, agrupada por impacto (`Añadido / Cambiado / Corregido / Obsoleto / Eliminado /
  Seguridad`). Reconstruirla del `git log` en el momento del release pierde la mitad.
- **Cambio incompatible de `regulus-proto` = major, sin excepciones**, y con nota de
  migración. Un cliente que no conecta y no sabe por qué es el peor fallo posible de este
  proyecto ([`06-stack.md` §5.1](06-stack.md)).

---

## 7. Cambiar las reglas

Las reglas son configuración, no código: así es como un harness cumple el principio 1
(*Flexible*) en vez de contradecirlo.

```bash
$EDITOR .gitconventions     # efecto inmediato, sin reinstalar nada
```

| Quieres… | Toca |
|---|---|
| un scope o tipo nuevo | `scopes` / `types` |
| que master esté cerrado de verdad | `on_protected_push = block` |
| asuntos más largos | `subject_max` |
| exigir scope siempre | `scope_required = true` |
| desactivar una comprobación | `run_*` a `never` |

Los hooks **no contienen literales de política**: si para cambiar una regla hay que editar
un `.sh`, eso es un bug del harness. Lo mismo vale para el presupuesto de tiempo
(`hook_time_budget`): un hook lento se acaba saltando, y un hook que se salta no existe.

---

## 8. Verificación del propio harness

```bash
scripts/commit-lint.sh -m "feat(core): añade el bus de eventos del core"   # pasa
scripts/commit-lint.sh -m "update stuff"                                    # falla
scripts/commit-lint.sh --last 10      # ¿está limpio el historial reciente?
scripts/secrets-scan.sh --all         # ¿hay credenciales versionadas?
```
