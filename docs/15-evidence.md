# 15 — Evidence: de dónde sale cada afirmación

> **v1.0** · 2026-11-10 · ✅ *Construido* (módulo `regulus/evidence`, plantilla
> `research-evidence`). Lee antes [`03-modules.md`](03-modules.md) (el contrato) y
> [`13-blocks.md`](13-blocks.md) §3.3 (`web`, `documents`, `memory`, de los que esto es vecino).
>
> **Nada de esto se ha corrido contra un modelo ni un buscador reales:** la plantilla se
> probó con un servidor OpenAI falso y una web local de prueba.

---

## 1. La idea

Un agente que investiga escribe frases. **Evidence guarda de dónde sale cada una** y comprueba,
sin modelo, que el origen existe y dice lo que se cita. No es un motor de investigación: es una
pieza genérica (fuentes, extractos, comprobación) que cualquier flujo puede usar, y una plantilla
que la monta junto a lo que ya existía.

```
 fuente = ubicación + texto que se le dio          entrada = afirmación + extractos
          └ versión = hash SHA-256 del texto                  └ cada extracto: fuente, versión,
                                                                 hash, trozo exacto, posición
        comprobación (determinista):  ¿existe la fuente?  ¿es la versión citada?  ¿está el extracto?
        NO comprobada:               que la afirmación sea cierta, ni que se siga del extracto
```

**Apoyo textual, no verdad.** Todo veredicto lleva `checked = "excerpt_inclusion"` y
`entailment = "not_checked"`. Que un extracto esté en una página no dice que la página sea
fiable ni que la afirmación se derive de él: eso lo juzga un modelo o una persona (un `judge`
después de `report`). Un informe puede pasar esta comprobación y estar equivocado.

## 2. Los datos

| Registro | Campos | Notas |
|---|---|---|
| **Fuente** (`Source`) | `id`, `location`, `title?`, `version`, `sha256`, `bytes`, `at`, `history[]`, `text` | `location` es una etiqueta (URL, ruta…): el módulo **no** la descarga ni verifica. `text` es lo que se le dio. `id` lo da quien registra, o `S<n>`; sin id, la misma `location` conserva su id |
| **Versión** | `version` (1, 2…), `sha256` del texto UTF-8 | cambia cuando cambia el texto. `history` guarda hash, tamaño, ubicación y hora de las anteriores (no su texto), hasta `max_versions` |
| **Entrada** (`Entry`) | `id` (`E<n>`), `statement`, `supports[]`, `at`, `by` | `by`: `@agente` por las herramientas, `node:<id>` por el bus |
| **Apoyo** (`Support`) | `source`, `version`, `sha256`, `location`, `excerpt`, `cited?`, `start`, `end` | `excerpt` es el **trozo exacto** de la fuente (offsets en caracteres, `end` excluido); `cited` solo aparece si lo que se citó difiere (`match = whitespace`) |

Los ids de `S<n>` y `E<n>` salen de un contador por colección que **nunca reutiliza**, y un id
olvidado (`forget`) queda **retirado**: tampoco se puede volver a dar a mano. Así `[E3]` de un
informe viejo no puede acabar señalando otra afirmación. El contador solo avanza cuando la escritura
ya no puede ser rechazada (no hay huecos por fallos).

### Estados de una comprobación

| Estado | Significa | Qué hacer |
|---|---|---|
| `valid` | la fuente existe, es la versión citada y el trozo está en su sitio | — |
| `stale` | el texto de la fuente cambió desde la cita (aunque el extracto siga ahí: `still_in_current` lo dice), o la fuente volvió a registrarse en **otra ubicación** | citar de nuevo contra la versión actual |
| `missing_source` | no hay esa fuente | registrarla o quitar la cita |
| `excerpt_not_found` | misma versión, pero el trozo no está en sus posiciones (registro alterado) | volver a citar |

Una entrada es `valid` si lo son todos sus apoyos; si no, manda el peor
(`missing_source` > `stale` > `excerpt_not_found`). **Cambiar el contenido nunca valida en
silencio una cita vieja:** el hash del apoyo no coincide con el de la fuente. Se compara el hash, no el
número de versión: si el texto vuelve a ser exactamente el citado (A → B → A) la cita **vuelve a
validar**, porque es el mismo texto en el mismo sitio; el número de versión solo informa.

## 3. Los nodos

Manifiesto: [`crates/modules/evidence/regulus-module.toml`](../crates/modules/evidence/regulus-module.toml)
(los campos están comentados ahí; la interfaz los dibuja sola).

```
 source ──▶ ┌──────────┐ ──▶ sourced   { id, version, sha256, created, changed, stale_entries… }
 cite   ──▶ │  store   │ ──▶ cited     { id, status, supports…, checked, entailment }
 forget ──▶ │          │ ──▶ forgotten { removed, kind, id, affected_entries }
            └────┬─────┘ ──▶ error     (por qué no se registró)
                 └─ tools ⇢ evidence_sources · evidence_read · evidence_cite · evidence_entries
 in ──▶ ┌──────────┐ ──▶ result  { valid, citations, problems, check, entailment }
        │  report  │ ──▶ report  texto + referencias (solo si pasó)
        └────┬─────┘ ──▶ error   los problemas, uno por línea (si no pasó)
             └─ tools ⇢ evidence_check
```

- **`store`** registra fuentes (`source`: `{ id?, location, text, title? }` o un array) y entradas
  (`cite`: `{ id?, statement, source, excerpt }` o `{ statement, supports: [{ source, excerpt }] }`).
  Con `strict = yes` (por defecto) una entrada cuyas referencias no validan **se rechaza** con la
  razón; con `no` se guarda con su estado, para auditar lo que afirmó un modelo.
- **`report`** lee un texto que cita entradas con marcadores (`[E1]`, el campo `marker`: una
  expresión regular con el id en el grupo 1). Pasa si cada marcador nombra una entrada que
  **valida ahora**, hay al menos `min_citations` entradas distintas y, con
  `require_paragraph_citations = yes`, cada párrafo largo cita una (la línea de un encabezado `#` está exenta; el resto de su bloque no). Un pase deja en `report` el
  texto más las **referencias**: por cada entrada, afirmación, ubicación (enlace si es http/https),
  fuente, versión, los 12 primeros caracteres del hash, posiciones y el extracto exacto. Un fallo
  no deja nada en `report` y sí las razones en `error`.
- **Un registro dañado en el almacén es un error explícito** que nombra la clave (no se ignora ni
  reinicia contadores): el libro se niega a seguir hasta repararlo. Los bloqueos se sueltan antes de
  emitir al bus.
- **Si el almacén falla** (disco lleno, permisos) el nodo da un error explícito; solo un core
  **sin** almacén (pruebas) usa la memoria del proceso. Nunca se degrada a memoria en silencio.
- **Estado**: en el almacén del módulo, de la base del proyecto del nodo, por `collection` y
  `scope` (`project` o `execution`). Los nodos que nombran la misma colección comparten libro.
  Sobrevive a la ejecución y al reinicio del core; otro proyecto no lo ve.

### Límites (campos del nodo `store`, sin valores escritos en el código)

| Campo | Por defecto | Al pasarse |
|---|---|---|
| `max_sources` | 200 | se rechaza la fuente nueva, nombra el campo |
| `max_entries` | 500 | ídem con la entrada |
| `max_source_bytes` | 524288 | se rechaza esa fuente |
| `max_total_bytes` | 8388608 | todo lo guardado, en bytes: por fuente texto + título + ubicación + por cada versión recordada su ubicación, hash y 24 de sus números; por entrada afirmación + extractos (citado y hallado). Se rechaza la escritura |
| `max_statement_chars` · `max_excerpt_chars` | 1000 · 1000 | se rechaza |
| `min_excerpt_chars` | 20 | se rechaza un extracto más corto: una palabra «valida» en cualquier sitio |
| `max_supports` | 6 | extractos por entrada |
| `max_title_chars` | 200 | se rechaza la fuente (título acotado) |
| `max_versions` | 5 | se olvidan las versiones más viejas del historial (no su efecto: seguirán `stale`) |
| `max_read_chars` | 4000 | lo que devuelve `evidence_read` de una vez |

**No hay expulsión silenciosa:** pasarse rechaza la escritura y dice qué campo; liberar sitio es
decisión de quien opera (`forget`).

## 4. Herramientas del agente (`tool@1`)

| Herramienta | Qué hace | Notas |
|---|---|---|
| `evidence_sources` | lista fuentes (id, ubicación, versión, tamaño) | sin texto |
| `evidence_read { source, start?, length? }` | lee el texto por trozos | `next_start` mientras quede; tope `max_read_chars` |
| `evidence_cite { statement, source, excerpt }` | registra una entrada | devuelve `cite_as` (`[E1]`); un extracto inventado es una **respuesta de error legible**, no una llamada rota. El agente **no elige el id** (sobrescribiría entradas ajenas). Se retira con `agent_cites = no` |
| `evidence_entries` | lista entradas con su estado ahora | |
| `evidence_check { text }` | el mismo control del `report`, para borradores | en el nodo `report`, con sus ajustes |
| `evidence_add_source` | el agente aporta una fuente | **solo con `agent_sources = yes`**: el texto sería lo que el agente escribió, y comprobar contra él no prueba nada del mundo. Por defecto `no` |

## 5. RPC públicos (`regulus call`)

Los tres sirven a **cualquier nodo del módulo**. Parámetros comunes: `{ execution_id, node_id }`. El
módulo guarda en el almacén (espacio global) un índice de los últimos 256 nodos que arrancaron, con su
configuración, así que encuentra un nodo de una ejecución terminada o de un core reiniciado.

Abrir el libro es otra cosa: el core solo da el almacén de un **proyecto** a través de una ejecución en
marcha de ese proyecto (`ModuleCtx` no tiene otra puerta, y no se cambió el core). Por eso:

- libro de un perfil **fuera de proyecto**: responde siempre, sin ejecución en marcha (probado tras reinicio);
- libro de un **proyecto**: con la ejecución viva, igual que siempre; si terminó, el error lo dice y se
  pasa `via: { execution_id, node_id }` de **cualquier ejecución en marcha del mismo proyecto** (se lee el
  libro del nombre pedido). `via` de otro proyecto lee el libro de *ese* proyecto: el módulo no puede
  comprobar que sean el mismo.

| Método | Extras | Devuelve |
|---|---|---|
| `evidence.list` | — | `collection`, `scope`, `used_bytes`, `limits` (solo `store`), `sources` (sin texto), `entries` (con estado ahora), `status_counts` |
| `evidence.inspect` | `{ source, text? }` o `{ entry, context? }` | fuente con `history` y `cited_by`; o entrada con cada apoyo comprobado y el texto alrededor (`context`, 80 caracteres por defecto) |
| `evidence.export` | `{ format? = "json" \| "markdown", include_text? = true }` | el libro completo |

### El export se verifica solo

`evidence.export` (`schema = "regulus.evidence.export/1"`) lleva las fuentes con su texto, las
entradas tal como se guardaron y los veredictos. Quien lo recibe recalcula, sin Regulus:

1. `sha256(source.text) == source.sha256` para cada fuente;
2. para cada apoyo: si `support.sha256 != source.sha256` → `stale`; si no, que los caracteres
   `start..end` del texto sean exactamente `support.excerpt`.

Eso reproduce `verdicts[].status`; las pruebas lo hacen. Con `include_text = false` el export
pesa menos pero ya no se puede recomprobar.

## 6. La plantilla `research-evidence`

Fichero: [`templates/research-evidence.toml`](../templates/research-evidence.toml). Un investigador
cuyo informe se comprueba contra extractos exactos de las páginas que leyó.

```
 self.question → [Agente: motor native ⇢ Proveedor] ─reply→ [report] ─report→ self.report
                    │ tools ⇢ web_search       (flow.tool ← workflow `search` ← motor web.search@1)
                    │         collect_source   (flow.tool ← workflow `collect`: URL → fetch →
                    │                           libro de evidencia + índice de documents)
                    │         evidence_*       (store, report)   search_documents · read_chunk
 self.seed → [store].source        (fuentes dadas a mano, sin web)
```

Se construye solo con módulos existentes (`agent`, `engine-native`, `providers`, `web`,
`documents`, `flow`) más `evidence`. **Sin shell ni ficheros:** las únicas herramientas son las de
las cajas cableadas, y el motor corre con `tool_policy = "deny"` salvo una lista cerrada
(`allow_tools`). Un `bash` que pida el modelo se rechaza (probado).

- **Es editable**: abrir `research-evidence` en el canvas enseña todas las cajas. Se sustituye el
  proveedor (`provider`), el buscador (`engine`: cualquier nodo que sirva `web.search@1`, o otro
  workflow `search`), el lector de páginas (`fetch` en `collect`), el agente (otro motor), los
  controles (`gate`).
- **El agente no puede inventar fuentes**: el texto lo trae `collect_source` (fetch) o `seed`.
- **Fallos que provoca**: referencia a una entrada inexistente, a una entrada `stale`, extracto
  que no está en la fuente (rechazado ya en `evidence_cite`), menos de `min_citations`.
- **Variables**: proveedor (`preset`, `base_url`, `credential`, `model`), `search_url`,
  `allow_private` (`no`: ni búsqueda ni fetch alcanzan direcciones privadas), `collection`,
  `match`, `min_excerpt_chars`, `max_sources`, `min_citations`, `require_paragraph_citations`.

### Pruebas

| Prueba | Qué demuestra |
|---|---|
| `crates/modules/evidence/src/model.rs` (unitarias) | localizar con/sin espacios y offsets en caracteres; cambiada, ausente o alterada nunca valida |
| `crates/modules/evidence/tests/evidence.rs` | núcleo real con almacén: fuentes versionadas, rechazos con motivo, informe con referencias, fuente que cambia → `stale`, export recomprobable, olvidar, límites por campo, **aislamiento entre proyectos y reinicio del core**, herramientas del agente |
| `crates/regulus-cli/tests/template_research_evidence.rs` | la plantilla aplica y `profile.check` sale vacío; contra un modelo falso y una web local: busca, recoge, cita dos extractos, el informe sale con referencias, un extracto inventado y una referencia desconocida fallan, no hay herramientas de shell, el libro se exporta y una página cambiada deja las citas `stale` |

## 7. Límites honestos

- **Solo apoyo textual.** Ver §1. La comprobación es inclusión de extracto; la coherencia de
  significado es de otro nodo.
- **La ubicación es una etiqueta.** Nada garantiza que el texto de una fuente sea el que hay en esa
  URL hoy (la plantilla lo trae de un `fetch`, y un módulo con acceso al bus puede darle otro).
- **El texto de la fuente es de confianza tal como se dio.** Una página puede contener
  instrucciones para el modelo (inyección); el control no las detecta.
- **No hay estructura dentro de la fuente** (páginas, secciones): los extractos se localizan por
  posición en caracteres del texto entregado. Un PDF o HTML convertido a texto se cita por su
  texto convertido.
- **El historial no guarda el texto viejo**, solo su hash: una cita `stale` no se puede
  recomprobar contra la versión original, solo saber que ya no coincide.
- **Un libro con `scope = project` es de todo el proyecto**: se comparte entre ejecuciones y un informe
  puede citar entradas de investigaciones anteriores (es lo buscado). Para un libro por ejecución,
  `scope = execution`.
- **Las referencias no inyectan enlaces**: la ubicación solo es un enlace si es una URL http(s) sin
  espacios ni `()[]<>` ni comillas; si no, va en un code span. El texto del extracto sí es el de la
  página, tal cual (una página puede traer Markdown suyo dentro del bloque citado).
- **Un lote es por elemento, no todo-o-nada**: en un array en `source` o `cite`, lo válido se
  registra y cada fallo sale en `error` y en `failed` del resultado; la llamada no falla, y reintentar
  el lote entero duplicaría.
- **Un marcador `[E1]` es una convención** del campo `marker`; un texto que cita sin marcador no
  se ve (para eso `require_paragraph_citations`).
- Sin búsqueda semántica propia ni reintento automático del agente tras un fallo: el reintento se
  cablea con los nodos que ya hay (el error de `gate` puede volver al agente como un prompt).
