# 18 — Nodos propios: escribir nodos en Python (y en Rust por stdio)

> **v0.2** · 2026-11-11 · Dos incrementos. **El primero** (2026-11-10, autorizado por
> Alexander): SDK de Python, `regulus node init`, campos `code` y `multiline`. ✅ Probado con
> procesos reales, contra un core real y, el inspector, en un navegador real; pasó una
> revisión independiente. **El segundo** (2026-11-11, candidato a 0.2.1, **sin publicar**):
> Alexander autorizó cerrar el editor, el parser sin ejecutar, recargar y las variantes.
> ✅ Construido y probado en el árbol de trabajo (protocolo **1.17**): editar la fuente de un
> módulo en la app, comprobarla sin ejecutarla, recargarla sin reiniciar el core, puertos
> que dependen de la config y sincronizar los puertos viejos (§2.1-§2.5, §4), y Rust por
> stdio con `node init --lang rust` y `node build` (§6). **El binario publicado 0.2.0 no lo
> trae:** habla 1.16. Lo que no se construyó y lo que sigue (§7) es 🔶 y **no está
> aprobado**. **No se ha usado aún en un proyecto real.**
>
> Código: `sdk/python/regulus.py` (SDK) y `sdk/python/templates/run.py` (el nodo inicial);
> `crates/regulus-proto/src/shape.rs` (la forma de un nodo), `crates/regulus-core/src/authoring.rs`
> (escribir un nodo, leer y guardar su fuente, comprobarla) y
> `crates/regulus-core/src/service/nodes.rs` (los métodos RPC); `crates/regulus-cli/src/node.rs`
> y `src/authoring.rs` (`regulus node …`); la app en
> `crates/regulus-tauri/frontend/src/components/code/` (el editor),
> `src/composition/authoring/` (panel de fuente, nodo nuevo, recarga, sincronizar) y
> `src/composition/editor/` (inspector). Ejemplos: `examples/local-modules/python-sdk/` y
> `examples/local-modules/rust-stdio/`.
> Pruebas: `sdk/python/tests/test_regulus.py` (en la CI), `crates/regulus-core/tests/authoring.rs`,
> `crates/regulus-cli/tests/node_init.rs` y `rust_node.rs`, y en la app
> `src/composition/authoring/*.test.ts` (uno contra un core real, si se le da uno).

---

## 1. Qué es

Un **nodo propio** es un tipo de nodo que escribe el usuario en su proyecto, sin tocar el
core ni recompilar. No hay runtime nuevo: es un **módulo local** (`[[modules.local]]`,
`03-modules.md` §6) hecho con un SDK que esconde el protocolo. El core lo trata como a
cualquier módulo: se veta, se quita con `--without` y la matriz de ablación lo cubre.

```
 «New Python node» (app) ─┐
 regulus node init ───────┴▶ tools/<id>/run.py + regulus.py
                                  │  [[modules.local]] en regulus.toml
   editar sin ejecutarlo ◀────────┤  (corre solo en un proyecto de confianza)
   source.get · check · save      │
                                  ▼
 inspector ──profile.save──▶ core ──JSON-RPC stdio──▶ python3 run.py
 (campos code)                 ▲ │  manifest · start · call · stop
                               │ └─ project.reload: arranca el proceso nuevo y solo
                               │    entonces para el viejo (si falla, sigue el viejo)
                               └──────────── emit ◀────────────
```

Lo que **no** es: ni un sandbox, ni un IDE (edita **un fichero**, el `.py` de un módulo
local), ni un mecanismo que sustituya a los módulos en Rust (los módulos de serie siguen
siendo crates, `plan.md`).

## 2. Escribir un nodo

```bash
regulus node init <carpeta-del-proyecto> --id caps   # tools/caps/run.py + regulus.py
regulus project open <carpeta> --trust               # o: regulus project trust <carpeta>
regulus module ls                                    # <proyecto>/caps@0.1.0   idle
```

`regulus node init <carpeta> --id <id> [--python <programa>]` no necesita el core. Pide
un proyecto que ya exista (`regulus.toml`), un id simple (`[a-z0-9][a-z0-9_-]{0,63}`) y que
`tools/<id>` no exista: **nunca sobrescribe**. Escribe `tools/<id>/run.py` (un nodo de
ejemplo `in → out` cuyo tipo es el id, con un campo `prefix`) y la copia del SDK, y añade la
entrada `[[modules.local]]` a `regulus.toml` como texto, para que sigan los comentarios y
las claves que no conoce; si eso no da exactamente la tabla de antes más la entrada,
reescribe la tabla (con aviso: se pierden los comentarios). Si algo falla, borra lo que
creó. **Nunca confía en el proyecto:** imprime el comando para hacerlo, y el `module_id` y
el `type` que lleva un nodo en un perfil. El SDK y el `run.py` inicial van **dentro del
binario** (`include_str!`): funciona con un binario descargado, sin el repo. Un solo
escritor lo hace todo: `regulus_core::authoring` (lo usan el CLI y el método `node.init` de
la app).

```python
import regulus as rg

@rg.node("caps", name="Caps",
         inputs=[rg.then_in("in", "text@1")],
         outputs=[rg.then_out("out", "text@1")],
         fields=[rg.field("prefix", "text", default="", label="Prefix")])
def handle(ctx, port, value):
    if not isinstance(value, str):
        raise rg.InvalidInput("text@1 must be a string")
    ctx.emit("out", ctx.config["prefix"] + value.upper())

if __name__ == "__main__":
    rg.run(version="0.1.0")
```

| Pieza | Qué hace |
|---|---|
| `node(type, name=, inputs=, outputs=, fields=)` | decorador: declara un tipo de nodo y su manejador. Errores de declaración (id vacío o repetido, puerto en la lista equivocada, valor que no es JSON) saltan **al importar**: el proceso muere y el módulo queda `broken` con su stderr. Todo tipo sale en el manifiesto con `managed_ports = true` (§2.2) |
| `then_in(id, capability, name=, required=, cardinality="many", lazy=, when=, repeat=)` | entrada "luego". `many` por defecto: varios cables la alimentan. `when` y `repeat` hacen que dependa de la config (§2.2) |
| `then_out(id, capability, name=, when=, repeat=)` | salida "luego" (con `when`/`repeat`, igual) |
| `field(id, type="text", default=, options=, when=, **extra)` | campo del nodo. `type` es abierto (`text`, `number`, `select`, `code`, `list`…). `when` lo oculta salvo que otro campo lo diga. Las claves de `extra` (`label`, `description`, `multiline`, `language`…) viajan tal cual en el manifiesto |
| `inspect(fuente) -> dict` | lee los tipos de nodo de un `run.py` **sin ejecutarlo** (§2.3). Lo usa el core; un autor no lo necesita |
| `handler(ctx, port, value)` | se llama por cada valor que llega. Lo que devuelve se ignora (avisa una vez por stderr) |
| `ctx.config` | la configuración del nodo sobre los `default` de sus campos (el core no aplica los defaults; lo hace el SDK, con una copia por evento) |
| `ctx.emit(port, value)` | pone un valor en una salida. Puerto no declarado, valor no JSON, texto que no es UTF-8 (un surrogate suelto de `os.fsdecode`) o línea más larga de lo que lee el core → `ValueError`, y no se envía nada |
| `ctx.execution_id` · `node_id` · `node_type` · `node` · `event` · `project_id` · `root` · `log()` | quién es, el `NodeEvent` crudo, el proyecto (de `start`) y una línea al log |
| `InvalidInput` / `NodeError` | errores a propósito: `-32602` (`InvalidParams`) y `-32000` (`Failed`) |
| `run(name=, version=, max_workers=)` | sirve hasta `stop` o hasta que se cierra stdin |

Copiar el SDK junto a cada `run.py` (lo hace `node init`) evita `PYTHONPATH` y deja el
proyecto autocontenido: cada proyecto fija la versión del SDK con la que se escribió. La
versión del SDK es solo la de `__version__` en `sdk/python/regulus.py` (un único sitio) y la
del core es la de `Cargo.toml`: coinciden en la release que corta las dos a la vez (0.2.1),
pero no hay obligación de que cada parche de la app cambie el SDK. Si la copia de un proyecto difiere de la del core, la comprobación avisa
(`sdk.outdated`) y nada más: no hay actualizador de copias.

### 2.1 El camino completo

```
 1 crear       «New Python node» (paleta) · regulus node init          tools/<id>/run.py
 2 confiar     «Trust project» (panel de fuente) · regulus project trust    sin confianza: blocked
 3 editar      panel de fuente · regulus node source | save | check    comprobar no ejecuta nada
 4 recargar    Reload · Save & reload · regulus project reload         el viejo sigue si falla el nuevo
 5 sincronizar «Review ports» · regulus profile sync [--apply]         nada se guarda sin confirmar
 6 ejecutar    Run · regulus run                                       el nodo recibe datos reales
```

En los comandos que hablan con el core (`node source|save|check`, `project reload`, `profile
check|sync`), `<proyecto>` es el **id** del proyecto abierto (`regulus project ls`; por
defecto, el nombre de la carpeta), no la ruta; `node init`, `node build`, `project open` y
`project trust` toman la carpeta.

Cada paso de la app es un método que el CLI también llama (principio 5). La app crea el
nodo en un proyecto abierto (`project.list`); sin ninguno (un hogar nuevo), el mismo diálogo
abre una carpeta por su ruta (`project.open`, como `regulus project open <carpeta>`), **sin
confiar en ella**: nada corre hasta *Trust project*. Probado de punta a punta contra un core real
(`crates/regulus-core/tests/authoring.rs`, `crates/regulus-cli/tests/node_init.rs` y el
test de la app `authoring/live.test.ts`) y en un navegador real (§4): crear → error de
sintaxis → arreglar → guardar → recargar → puertos → cablear → ejecutar.

### 2.2 Variantes: puertos que dependen de la configuración

Un puerto puede existir solo según la config del nodo. En Python:

```python
@rg.node("router", name="Router",
         inputs=[rg.then_in("in", "text@1")],
         outputs=[rg.then_out("case_{item}", "text@1", repeat="cases"),
                  rg.then_out("extra", "text@1", when={"field": "mode", "equals": "split"})],
         fields=[rg.field("cases", "list", default=["a", "b"]),
                 rg.field("mode", "select", default="plain", options=["plain", "split"]),
                 rg.field("volume", "number", default=1,
                          when={"field": "mode", "equals": "split"})])
```

En un manifiesto TOML, los puertos condicionales van en `[[node.dynamic_port]]`
(`[[node.port]]` no cambia: ningún cliente ni la TUI se rompe) y el tipo lleva
`managed_ports = true`:

| Clave | Qué hace |
|---|---|
| `[[node.dynamic_port]]` | mismas claves que `[[node.port]]` más `when` y/o `repeat`. Primero van los fijos y luego los dinámicos, en orden. Dos dinámicos pueden compartir `id` si sus `when` se excluyen (una variante que cambia el tipo de un puerto); si valen a la vez, gana el primero y se avisa |
| `when = { field = "mode", equals = "split" }` | el puerto o el campo existe solo si el valor del campo (la config, si no su `default`) es igual; también `{ field, in = ["a","b"] }` o `{ field = "advanced" }` (verdadero: `true`, texto o lista o tabla no vacíos, número distinto de cero) |
| `repeat = "<id de un campo>"` | un puerto por elemento de ese campo (`type = "list"`, lista de texto). El `id` (y `name`, si hay) llevan `{item}`; el elemento tiene que casar `[A-Za-z0-9_-]{1,64}` y no repetirse (si no, se avisa y se salta); `when` se aplica primero |
| `[[node.field]]` con `when` | el campo se oculta en los clientes y su valor se conserva |
| `[[node.field]]` con `type = "list"` | lista de texto; un cliente que no lo conoce la enseña cruda |
| `managed_ports = true` | todo nodo de este tipo tiene exactamente los puertos que da su forma; una diferencia está **vieja** (§2.5). El SDK de Python lo pone siempre. Los módulos enlazados no lo ponen: sus nodos guardan subconjuntos de puertos hechos a mano (medido: 211 de 317 nodos de las plantillas difieren de su manifiesto) |

**Una sola función decide los puertos:** `regulus_proto::shape::shape(tipo, config)`,
en el core. La app la pide con `module.shape { module_id, type, config? }` al crear un nodo
y, con una pausa (`shapeDelayMs`), cuando cambia un campo que un `when` o un `repeat`
nombra; el CLI, con `regulus module shape <módulo> <tipo> [--config JSON]`. Devuelve
`{ ports, hidden_fields, problems }`; un puerto sin nombre toma su id (lo mismo que hace el
core al cargar y guardar un perfil, así un nodo creado desde la forma no queda viejo tras
guardar); es puro y rápido, sin E/S. **Los clientes no llevan un segundo resolutor.** La
TUI llama a la misma función al crear un nodo, con los defaults de sus campos, así que un
tipo con puertos dinámicos nace con los suyos; no vuelve a resolverlos si luego se edita un
campo que alimenta la forma, ni oculta los campos con `when` (para lo primero, `regulus
profile check` y `regulus profile sync`).

La comprobación de una declaración (`regulus_proto::shape::validate`, los mismos mensajes
en todas partes) rechaza ids de puerto o de campo repetidos, un `repeat` que nombra un
campo inexistente o que no es `list`, un `id` repetido sin `{item}` (o `{item}` fuera de un
`repeat`), un `when.field` inexistente, `equals` junto a `in`, y un puerto dinámico con el
id de uno fijo. Un módulo local cuyo manifiesto no la pasa queda `broken` con esos
mensajes (y una recarga conserva el anterior).

**Un módulo con puertos dinámicos declara `requires_proto = "^1.17"`.** El SDK lo hace solo
(`"^1"` si no hay ninguno); en un manifiesto escrito a mano hay que ponerlo. Así un core
1.16, que descartaría `dynamic_port` en silencio, lo deja `incompatible` en vez de cargarlo
sin esos puertos. En marcha, `ctx.emit` y la comprobación de entradas usan los puertos del
**nodo** que llega en la llamada (así valen `repeat` y `when`); un nodo sin puertos propios
cae a lo declarado.

**No está:** `capability = "$T"` (un genérico resuelto por el primer cable), mínimo y
máximo de puertos que crecen, y variantes de la TUI (§7).

### 2.3 Editar la fuente sin ejecutarla

Solo se alcanza **el `.py` de un `[[modules.local]]` de un proyecto abierto**: el primer
argumento de `command` que acaba en `.py` (relativo al `cwd` de la entrada, que por defecto
es el proyecto), o `command[0]`. Tiene que ser relativo, sin `..`, dentro del proyecto una
vez resueltos los enlaces, y no ser él mismo un enlace. No se alcanza otra ruta (ni la
copia del SDK ni `regulus.toml`). Leer y guardar **no ejecutan nada ni piden confianza**.
Texto UTF-8, como mucho `max_source_bytes`.

| Método · comando | Qué hace |
|---|---|
| `source.get { project_id, module }` · `regulus node source <proyecto> <módulo>` | `{ path, file, language, text, revision }` |
| `source.save { project_id, module, text, revision, force? }` · `regulus node save … <fichero> --revision R` | escribe de forma atómica (fichero temporal y rename, conserva permisos). `revision` es lo que el cliente leyó (`null` = "espero que no haya fichero": lo crea). Si cambió y no hay `force`: **`-32015` SOURCE_CONFLICT** con la revisión actual en `data`, y no se escribe nada. **No recarga** |
| `source.check { project_id, module, text? }` · `regulus node check … [--file F]` | comprueba el texto (un borrador sin guardar) o el fichero |

La revisión es opaca: se compara como texto, nunca se interpreta. `-32016`
SOURCE_UNAVAILABLE: no hay ese módulo local, no tiene un `.py` editable, la ruta sale del
proyecto o es un enlace, no existe, no es UTF-8 o es demasiado grande.

**Comprobar no ejecuta nada del proyecto.** El `python` de `[local_modules] python` de
`config.toml` (**nunca** el `command` del proyecto), con `-I -S -c <el lector del SDK del
core>`, el texto por stdin, el entorno vacío salvo `PATH`, otro `cwd` y un tiempo máximo
(`check_timeout_ms`). El lector es una lista blanca: literales, listas, dicts, tuplas,
menos unario, nombres de módulo ligados una vez a algo así, `import regulus [as x]` /
`from regulus import …` y alias simples (`node = rg.node`), y las llamadas a `then_in`,
`then_out` y `field` que evalúa **la propia copia del SDK del core**. Cualquier otra cosa
(otras llamadas, lambdas, f-strings, `**kwargs`, cadenas de atributos) sale como aviso
`python.unreadable` y no se evalúa nunca. El fichero tampoco puede hacer trabajar al lector
sin límite: cada nombre se lee una vez y cuenta su coste entero cada vez que se usa, y un
solo presupuesto por fichero, de 100 000 valores (`_MAX_VALUES`; un nombre que se dobla en
cada línea llega en 17), 4 MiB de texto (`_MAX_BYTES`; una cadena larga usada muchas veces
serían otras tantas copias en el manifiesto) y 64 niveles de anidamiento, para antes de
copiar o serializar nada, con `python.unreadable` y el motivo
(`test_names_that_double_at_every_line_are_read_within_a_budget`,
`test_long_text_used_many_times_is_read_within_a_byte_budget_in_capped_memory`). Los
ficheros de la revisión tardan 0,1-0,2 s y ~20-30 MB en vez de agotar los 5 s o la memoria
(~600 MB y `MemoryError`). Probado con un fichero centinela y un módulo
importable plantado (`test_malicious_sources_run_nothing_even_through_the_core_command`).

`SourceCheck` = `{ language, checked, interpreter, diagnostics, nodes, complete,
matches_loaded }`. Líneas y columnas desde 1, en puntos de código Unicode. `nodes` son los
tipos de nodo leídos de forma estática; `complete = false` si un argumento no es literal;
`matches_loaded` dice si coinciden con los del módulo cargado (`null` si no está cargado o
la lectura es incompleta), que es el "recarga para aplicar".

| Código | Severidad | Cuándo |
|---|---|---|
| `python.syntax` | error | el parser, más un `compile()` del árbol **sin ejecutarlo** para lo que solo rechaza la carga: `return` o `await` fuera de función, `break` fuera de un bucle, un argumento repetido, `nonlocal` a nivel de módulo |
| `node.declaration` · `shape.invalid` | error | el SDK fallaría al importar · la declaración no pasa `shape::validate` (§2.2) |
| `python.unreadable` | aviso | un argumento de decorador no es un literal; esa parte no se lee |
| `python.handler` · `python.no_nodes` · `python.no_run` | aviso | manejador con menos de 3 parámetros o `async def` · ningún `@rg.node` · falta `rg.run(...)` |
| `sdk.outdated` | aviso | la copia `regulus.py` del proyecto difiere de la del core |
| `check.unavailable` | aviso | falta el intérprete, se pasó el tiempo o murió; `checked = false` y `nodes` vacío: **no se afirma que sea válido** |

Límite honesto: la gramática es la del Python del anfitrión; un proyecto que usa un Python
más nuevo puede diferir. En la app hay además una primera capa, local e inmediata, en el
editor (§4): no sustituye a esta.

### 2.4 Recargar sin reiniciar el core

`project.reload { id, module?, stop_executions? }` · `regulus project reload <id-del-proyecto>
[--module <id>] [--stop-executions]`. `module` es el id local o `<proyecto>/<id>[@versión]`;
sin él, todos los `[[modules.local]]` del `regulus.toml` actual (con las entradas añadidas o
quitadas desde que se abrió). Por módulo, de forma transaccional: el proceso nuevo arranca,
dice su manifiesto, se valida su forma y responde `start`; **solo entonces** se cambia en el
registro y se para el viejo. Si algo falla, **el viejo (si servía) sigue** y el resultado
dice por qué. Un proyecto sin confianza da `blocked`, sin proceso. Los perfiles del proyecto
y su id siguen pidiendo cerrar y abrir (`11-composition.md` §13).

El resultado trae, por módulo, `outcome`: `started` · `replaced` · `kept` (el nuevo falló y
el viejo sigue; con `error`) · `failed` (no había nada que servir) · `blocked` · `removed` ·
`skipped`; los tipos de nodo añadidos, quitados y cambiados; las ejecuciones paradas; y
`stale`, los nodos de perfiles cargados que ahora tienen los puertos viejos (§2.5). Los
clientes piden otra vez `module.list` y `module.manifest` (la paleta se refresca).

**Ejecuciones vivas.** Antes de cambiar, el core busca ejecuciones vivas o reanudándose con
un nodo de ese módulo. Si las hay y no se pidió `stop_executions`: **`-32014`
MODULE_IN_USE**, no cambia nada y `data` lista las ejecuciones (`id`, `profile_id`,
`workflow_id`, `nodes`). Con `stop_executions: true` el core las para (quedan `stopped`,
como con `execution.stop`) y entonces cambia. **Una ejecución nunca pasa de un proceso viejo
a uno nuevo.** Si la recarga ya había parado ejecuciones y luego falla, el error lleva
`data.stopped_executions` con las que paró, para que el cliente lo diga.

**Cerrojos.** Solo una recarga (o `node.init`) a la vez; mientras arrancan los candidatos
(hasta `start_timeout_ms`) nada más espera: `module.list`, `profile.save`, `source.check`,
las ejecuciones y los eventos siguen. El cambio toma un instante los cerrojos de escritura
de perfiles y de ejecuciones, y vuelve a comprobar que el proyecto sigue abierto en la
misma carpeta, que cada módulo es el planeado (si no, `-32003`, no cambia nada, "recarga
otra vez") y las ejecuciones vivas (si una empezó mientras arrancaba el candidato,
`MODULE_IN_USE`). Los procesos viejos se paran con todos los cerrojos ya soltados.

**Medido** (2026-11-11, build debug, en proceso, 50 muestras): `module.shape` 73 µs de
mediana, `profile.sync` 132 µs, `profile.check` 65 µs, `source.get` 54 µs, `project.get`
83 µs. Crear → error → arreglar → recargar → sincronizar → ejecutar con Python real: ~0,5 s.
Durante una recarga con un candidato que tarda 1,5 s en arrancar: `module.list` 0,2 ms,
`profile.save` 0,5 ms, `source.check` 159 ms.

### 2.5 Puertos viejos y sincronizar

`profile.check` añade tres **avisos** (el perfil sigue siendo válido y se puede guardar):

| Código | Cuándo |
|---|---|
| `node.ports_stale` | un nodo de un tipo `managed_ports` cuyos puertos no son los de su forma (por id, sin orden, comparando todas las claves; un nombre vacío vale su id). Dice qué falta, qué sobra y qué cambió |
| `node.type_missing` | el módulo del nodo está cargado y ya no tiene ese tipo (p. ej. tras una recarga) |
| `node.shape_problem` | la forma avisó de algo para la config de ese nodo |

`profile.sync { profile_id? | profile?, workflow_id?, node_ids? }` es una **vista previa y
no escribe nunca** (`profile` gana a `profile_id`, para sincronizar un documento sin
guardar). Sin `node_ids` mira todos los nodos viejos; con ellos, esos nodos, aunque no
sean `managed_ports` ("reponer los puertos a los del módulo"). Devuelve el perfil con los
puertos nuevos y sin los cables que caen, `drops_wires` y, por nodo, `added`, `removed`,
`changed` y `wires_removed` con su motivo. **Un cable cae si toca un puerto quitado o uno
cuya `direction`, `kind` o `capability` cambió;** un cambio de `name`, `required`,
`cardinality` o `lazy` conserva los cables (`profile.check` ya dirá, p. ej.,
`port.too_many_wires`). Un nodo cuyo módulo o tipo falta se lista con su problema y no se
toca.

**Aplicar es el segundo paso explícito del cliente.** App: un cambio único del documento,
con Deshacer, tras confirmar los cables que caen; no se guarda hasta Guardar. Una vista
previa solo se aplica sobre el documento para el que se pidió: si ya es otro perfil se
descarta (con aviso), y si el documento cambió desde entonces se pide otra al core antes de
enseñar o aplicar nada. CLI: `regulus profile sync <id>` imprime el plan; `--apply` guarda
y se niega si cae algún cable salvo que se pase también `--drop-wires`.

### 2.6 Configuración

`config.toml`, sección `[local_modules]` (las demás claves, en `03-modules.md` §6):

| Clave | Por defecto | Qué |
|---|---|---|
| `python` | `"python3"` | el intérprete del anfitrión que usa `source.check`, y el programa por defecto de las entradas que escribe `node.init` |
| `check_timeout_ms` | `5000` | cuándo se mata la comprobación |
| `max_source_bytes` | `1048576` | tope para leer, guardar y comprobar una fuente |

Los ajustes de la app (tiempos del editor y de la comprobación) son del dispositivo y
están en `04-client.md` §13.

## 3. Cómo cumple el protocolo

Verificado contra `crates/regulus-core/src/process.rs` y el ejemplo `echo`:

| Mensaje | El SDK |
|---|---|
| `manifest` | responde un objeto JSON con la forma de `regulus-module.toml`: `requires_proto = "^1"` (`"^1.17"` si algún tipo tiene puertos dinámicos, §2.2), `managed_ports = true` en cada tipo, `name` si se da, y `id = "local/sdk@<version>"` solo para que el core lea la versión (el id real lo pone el core: `<proyecto>/<id>@<versión>`) |
| `start` | guarda `project_id` y `root` para `ctx`; responde `null` |
| `call` → `node/event` | valida tipo de nodo y puerto de entrada (`-32602` si no), llama al manejador; los `emit` se escriben **antes** de la respuesta, así el core los publica antes de que la llamada vuelva y siguen su cadena (con una sola llamada en vuelo, `bus.max_depth` ve los bucles: §5) |
| `call` → `node/start` · `node/stop` · `node/child` · `node/hook` | `-32601` al momento, en el hilo que lee, sin esperar a un trabajador libre: el core lo lee como "nodo sin ciclo de vida" y no retrasa arrancar ni parar una ejecución |
| error del manejador | `InvalidInput` → `-32602`; `NodeError` y cualquier otra excepción, **también `sys.exit()` y `KeyboardInterrupt`**, → `-32000` con la traza en stderr. Ni el proceso ni el hilo trabajador mueren. Un mensaje que no es UTF-8 se manda escapado (`\udcff`) |
| `stop` | responde y termina; lo que siga corriendo muere con el proceso |
| otra petición | `-32601`. Notificaciones, respuestas y líneas que no son JSON se ignoran (estas últimas, con aviso en stderr) |

**Stdout es solo del protocolo.** `run()` duplica los descriptores 0 y 1 para el SDK (no
heredables por hijos), apunta el 1 a stderr y el 0 a `/dev/null`. Un `print`, un
`subprocess` que escribe en stdout o una librería en C que escribe en el descriptor 1 van
al log del core, no rompen el protocolo; un `input()` lee vacío en vez de robar mensajes.
Lo primero que escribe es una línea vacía (el core se la salta): así un `print(...,
end="")` hecho antes de `run()` no se pega a la respuesta del manifiesto. Las líneas se
escriben enteras, en UTF-8 estricto, bajo un cerrojo: con hilos no se mezclan.

**Lo que el core no podría leer no sale.** El core descarta una línea con un surrogate
suelto (Python lo escaparía como `\udcff`; `serde_json` no lo acepta) y mata el proceso que
escribe una línea de más de `max_line_bytes`. Por eso el SDK lo comprueba antes de
escribir: un `emit` así es un `ValueError` del manejador (un error de esa llamada, no un
`emit` perdido ni el módulo muerto). El tope es 16 MiB, el defecto del core; si el core usa
otro, se dice con `REGULUS_MAX_LINE_BYTES` en el `env` de la entrada.

**Concurrencia.** El core manda peticiones concurrentes (una por ejecución que entrega a la
vez) y solo espera la de cada una. Un bucle en serie haría que un evento lento de una
ejecución frenara a las demás. El SDK atiende `call` en **`max_workers` hilos** (4 por
defecto; `run(max_workers=…)` o `REGULUS_MAX_WORKERS` en el `env` de la entrada de
`regulus.toml`). Dentro de una ejecución el orden lo pone el core (no entrega el siguiente
evento hasta que vuelve la llamada). Con `1`, todo en orden de llegada. Si los manejadores
comparten estado, lo protegen ellos.

**Medido** (2026-11-10, máquina de desarrollo con compilaciones en paralelo): `manifest`
en 70-130 ms desde el arranque de Python; 1000 eventos encadenados, ~0,35 ms por evento.
Un valor de 4 MiB de ida y vuelta, por debajo de 2 s con stdin con búfer (el test lo
exige; leído byte a byte tardaba ~4-5 s). El core arranca los locales en paralelo y espera
al más lento (`03-modules.md` §6).

**Comprobado contra un core real** (binario de desarrollo del árbol actual, con
`Field.extra`, 2026-11-10): sin confianza el módulo sale `blocked`; con `project trust`,
`idle` con la versión del manifiesto, y `module show` enseña `language`, `label` y
`description` del campo; un perfil `template → lines` transforma `"hello"` en dos líneas y
`count = 2`; `serve --without <proyecto>/sdk` deja el resto igual; al cerrar el proyecto
no queda proceso. El test de punta a punta del repo es el de `node init`
(`crates/regulus-cli/tests/node_init.rs`: init → `blocked` → trust → manifiesto → perfil
→ valor transformado → veto → cierre sin proceso). Los tests del SDK corren en la CI.

## 4. El editor: campos de código y la fuente del módulo

El manifiesto conserva las claves desconocidas de un campo (`Field.extra`, `serde(flatten)`
en `crates/regulus-proto/src/manifest.rs`): salen hacia los clientes planas, tal como las
escribió el módulo, y el core no las lee. Un cliente antiguo que no conoce un tipo lo
muestra con el control `raw`.

| Pista del campo | Qué hace en la app |
|---|---|
| `label` · `description` | nombre visible junto al id, y una línea de ayuda debajo |
| `language` | se ve junto al tipo (`code · python`). El resaltado y la comprobación de sintaxis existen para `python`; otro valor se edita como texto |
| `indent` | lo que mete Tab en un `code`: `"tab"` o de 1 a 8 espacios (4 por defecto) |
| `multiline` | solo en `type = "text"`: área de texto que ajusta líneas (3 a 10); Tab cambia de foco. **Sigue siendo un `<textarea>`**, sin editor |

### 4.1 Campos `code`: CodeMirror y borrador local

Un campo `type = "code"` se edita con **CodeMirror 6**, cargado con `import()` al abrir un
campo `code` o una fuente (un trozo aparte que la primera pantalla no paga; las versiones
viven solo en `06-stack.md` §2). Si ese trozo no carga, vuelve el área de texto de antes.
Resalta Python, numera líneas, marca los paréntesis que casan y crece de 4 a 18 líneas
(`codeMinLines`, `codeMaxLines`) y luego se desplaza.

- **Borrador local.** El texto vive en el editor, no en el documento del perfil: teclear no
  rehace el grafo entero. El editor se lo entrega al perfil tras una pausa
  (`codeCommitMs`), al perder el foco o cuando algo lo pide (**vaciar**, `flushEdits()`).
  Guardar (`Ctrl/Cmd+S`), Comprobar, Ejecutar, cambiar de perfil y aplicar una plantilla
  vacían primero: lo que se guarda es lo que se ve. La primera tecla ya marca el perfil
  como sin guardar, y una entrega tardía solo cae en el mismo perfil y el mismo nodo.
- **Deshacer.** `Ctrl/Cmd+Z` y rehacer son los del editor y cubren teclas y Tab (a
  diferencia del área de texto del navegador, que no deshacía un Tab). Probado: 10 veces
  `Ctrl+Z` vuelven al texto original y rehacer lo repone.
- **Teclado.** Tab sangra (cada línea si la selección abarca varias), Shift+Tab quita
  sangría y **Esc y luego Tab sale del campo**, para que el teclado no quede atrapado.
- **Sintaxis sin ejecutar.** Tras una pausa (`codeLintMs`) el editor marca los errores de
  sintaxis con el parser incremental de CodeMirror (Lezer): es lo que se ve al instante, y
  es una aproximación del parser del editor. En una fuente de módulo se suman los problemas
  del core (§2.3), con línea y columna en puntos de código convertidos a posiciones del
  editor y desplazados con lo que se edite desde la comprobación; hacer clic en uno mueve el
  cursor a su línea. Ninguna de las dos capas ejecuta el código.
- **Lo que no hace:** autocompletar, ni el aviso de `{{nombre}}` (abajo), ni TUI: la TUI
  no dibuja los `code` todavía (un `code` sale como texto crudo de una línea y `multiline`
  se ignora; el texto con saltos se edita desde la app o en el fichero del perfil).

Ojo: un `{{nombre}}` dentro del código de un campo es un hueco de plantilla para
`template.apply` y `template.save` (`11-composition.md` §7). Si no está declarado, falla; el
editor no avisa antes.

### 4.2 El panel de fuente

Un panel junto al canvas edita el `.py` de **un** módulo local (`source.get`, §2.3):

- **Comprobar** (`source.check`, sola tras `sourceCheckMs` de pausa; con `0` solo a
  petición o al guardar) enseña los problemas del core y la lista de nodos leída de forma
  estática; si no coincide con el módulo cargado dice "recarga para aplicar".
- **Guardar** (`source.save` con la revisión leída). `Ctrl/Cmd+S` dentro del editor guarda
  el fichero, no el perfil. Si el fichero cambió fuera, sale un aviso con tres salidas:
  *cargar el suyo* (se pierde la edición), *sobrescribir* (`force`) o *seguir editando*.
- **Guardar y recargar** (§2.4). Si hay ejecuciones vivas, un diálogo las lista: *mantener*
  (no cambia nada) o *parar N y recargar* (`stop_executions`); el resultado dice cuáles se
  pararon. El resultado de cada módulo se dice con palabras.
- Un proyecto sin confianza muestra el módulo `blocked` y la acción **Trust project**.
- El borrador de la fuente no forma parte del vaciado del perfil: es otro documento, con su
  propio guardar. **Se conserva** mientras no se guarde ni se descarte: no se pierde al
  cambiar de vista (Operar, Plantillas, Ajustes) y volver, y si el fichero cambió fuera
  mientras tanto, al volver sale el aviso de conflicto de arriba. Pedir **otra** fuente con
  cambios sin guardar (el «Edit source» de otro módulo, un nodo nuevo, Ajustes → Módulos)
  no la reemplaza: la deja pendiente con un aviso de tres salidas, *Guardar y abrir*,
  *Descartar y abrir* o *Seguir editando*. Recargar la página o cerrar la ventana con
  cambios sin guardar, de la fuente **o** del perfil, pregunta antes desde cualquier vista
  (`beforeunload` y el botón de cerrar de la barra de título). No pregunta un cierre que
  manda el gestor de ventanas (Alt+F4), igual que antes con el perfil. Probado en un
  navegador (2026-11-11): cambiar de vista y volver, otra fuente desde el inspector y desde
  Ajustes → Módulos con sus tres salidas, salir de la página justo tras teclear, y volver
  con el fichero cambiado fuera. Medido tras este cambio: mismo coste por tecla que antes,
  dentro del ruido (la tecla no escribe nada fuera del editor; solo la primera tras una
  pausa anota que hay borrador).
- Se abre desde el inspector de un nodo de un módulo local («Edit source»), al crear un
  nodo, y desde Ajustes → Módulos (§10 de `04-client.md`).

### 4.3 Probado y medido

**Probado en un navegador real** (2026-11-11, Chromium contra un core real, build de
producción): el recorrido de §2.1 completo, incluido el error de sintaxis marcado al
instante en el editor y por el core, el conflicto de escritura con sus tres salidas, el
diálogo de ejecuciones vivas, sincronizar con Deshacer y con cables que caen, un campo con
`when` que se oculta, y un core 0.2.0 (protocolo 1.16) sin errores de consola, donde
«Edit source» y el aviso de puertos no aparecen y «New Python node» dice que necesita 1.17.
También teclado y deshacer en un campo `code` (escribir, Tab, Shift+Tab, `Ctrl+Z`, rehacer,
Esc → Tab) y que el valor sobreviva a Guardar y a recargar la página. El incremento de
2026-11-10 se había probado con 36 comprobaciones sobre el área de texto.

**Medido, y el objetivo no se cumple** (2026-11-11, build de producción, 30 teclas separadas
150 ms con el cursor a media altura, de la tecla al siguiente cuadro, así que incluye
esperar a ese cuadro; **la máquina compartía CPU con compilaciones de otros agentes**, así
que los números son ruidosos y solo se comparan dentro de una misma sesión):

| Caso | Mediana | p90 |
|---|---|---|
| Campo `code` de 1000 líneas, editor nuevo (pasada 1 · pasada 2) | 12,9 · 19,9 ms | 26,4 · 30,9 ms |
| El mismo campo con el área de texto anterior (misma sesión, pasada 1) | 24,0 ms | 41,2 ms |
| Campo `code` de 2 líneas, editor nuevo · área de texto anterior | 10,1 · 41,7 ms | 21,2 · 74,0 ms |
| Panel de fuente, `run.py` de 1031 líneas (1000 de comentarios), cuadro | 37,3 ms | 57,5 ms |
| Actualización de CodeMirror por tecla: 1000 líneas de código · 1000 líneas de comentarios | 6,4 · 23,1 ms | 26,2 · 25,9 ms |
| Primera pantalla útil (6 cargas): base 0.2.0 · nueva | 452 · 440 ms | — |

- **El presupuesto de 16 ms de `00-principles.md` §3 no se alcanza en el p90** (26-31 ms con
  1000 líneas, 21 ms con 2) y la mediana solo lo cumple en una de las dos pasadas. **No se
  promete 16 ms**; el presupuesto no se ha tocado. Mejora lo anterior en la misma sesión
  (la base medía 24-42 ms de mediana según el tamaño), pero no lo cumple.
- El tiempo por tecla es el parseo de Lezer dentro de la actualización síncrona de
  CodeMirror (acotada a ~20 ms por él), no React: ya no hay ninguna renderización de React
  por tecla. Un tramo largo de **solo comentarios** se reparsea entero en cada tecla (~23 ms);
  el código normal, ~6 ms.
- La primera pantalla sigue por debajo de 1 s (mediana 440 ms contra 452 ms). El trozo
  principal crece 8,5 kB (2,4 kB comprimido); CodeMirror es otro trozo, cargado al abrir un
  campo `code` o una fuente. Abrir una fuente (clic en «Edit source» hasta el editor
  listo): 660-780 ms contra el core de desarrollo.
- La línea base del incremento anterior, con el área de texto y menos carga en la máquina,
  fue de ~19 ms de mediana en el campo de una línea y de ~29-30 ms (p90 ~40-43 ms) con
  1000 líneas.

## 5. Límites, dichos sin adornos

- **No hay sandbox.** El proceso corre con los permisos del usuario. Lo que protege es la
  confianza en el proyecto (`03-modules.md` §6) y el entorno vacío (`env_passthrough`).
  El aislamiento de ejecución sigue fuera de v1 (`11-composition.md` §12).
- **Del `ModuleCtx` solo hay `emit`.** Sin `store`, `checkpoint`, `node_dir`, `resolve`,
  `call` ni `call_workflow`; sin ciclo de vida por nodo. Añadirlos es conversación de
  diseño (§7, F6).
- **Solo puertos "luego".** Un nodo Python no sirve ni usa capacidades por cable "usa".
- **Recargar cambia el proceso, no las ejecuciones en marcha** (§2.4). Una ejecución viva
  que usa el módulo impide la recarga (`-32014`) o se para si se pide; nunca pasa a un
  proceso nuevo. Los perfiles del proyecto, su id y las carpetas de perfiles siguen pidiendo
  cerrar y abrir el proyecto.
- **Los nodos ya puestos guardan sus puertos.** Si el manifiesto cambia salen como viejos
  (`node.ports_stale`, §2.5) y se sincronizan a petición. La app solo sincroniza sola cuando
  el usuario edita un campo del que depende la forma y no cae ningún cable; tras abrir o
  recargar espera a que se revise.
- **La comprobación de sintaxis usa el Python del anfitrión** (§2.3): un proyecto que usa
  otro más nuevo puede diferir. Si no puede correr, dice `check.unavailable` y no afirma
  nada.
- **Solo se edita un fichero por módulo local**, el `.py` de la entrada. No hay árbol de
  ficheros, git ni editor externo («abrir en el editor» no existe), y tampoco vista del
  stderr del módulo en Ajustes (el motivo de un fallo de arranque o de recarga sí sale, con
  las últimas líneas que el core guarda).
- **Una línea de más de `max_line_bytes` (16 MiB)** mataría el proceso; el SDK la rechaza
  antes como error del manejador, pero solo conoce el tope que se le dice (§3).
- **Una llamada colgada** retiene el bus de esa ejecución hasta `call_timeout_ms`. Y como un
  hilo de Python no se puede matar, el manejador sigue ocupando su trabajador después de
  que el core se rinda: con `max_workers = 1`, el módulo entero deja de atender eventos
  (los `node/start` y `node/stop` sí se siguen respondiendo).
- **`bus.max_depth` con varias llamadas en vuelo.** Límite del core, de antes: los `emit`
  de un proceso entran en una cola compartida y los publica el hilo del core que esté
  esperando, y la profundidad se cuenta por hilo. Si dos ejecuciones esperan a la vez al
  mismo módulo, un `emit` de una puede publicarlo el hilo de la otra y su cadena empieza
  en 0: un bucle que pasa por un nodo Python puede escaparse del tope. Con una sola llamada
  en vuelo no pasa. Arreglarlo es entregar cada `emit` a la llamada de su `execution_id`
  (solo `process.rs`, sin API nueva): 🔶 propuesto, no aprobado.
- Python 3.9 o más reciente, solo biblioteca estándar (sintaxis comprobada para 3.9; los
  tests corren con 3.11 y 3.14, y en la CI con el `python3` del runner).

## 6. Rust por stdio, y los tres caminos para añadir un módulo

**Tres caminos, que no se mezclan:**

| | Módulo estático (crate enlazado) | Módulo local por stdio | Nodo con el SDK de Python |
|---|---|---|---|
| Qué es | un crate en `crates/modules/<id>` y una línea en `crates/regulus-cli/src/modules.rs` | cualquier programa que hable el protocolo (`03-modules.md` §6): Rust con el ayudante `sdk/rust/regulus.rs`, Go… | `sdk/python/regulus.py` escondiendo el protocolo |
| ¿Binario `regulus` nuevo? | **sí**: se vuelve a enlazar; el registro es a mano | no | no |
| `ModuleCtx` | completo | solo `emit` | solo `emit` |
| Confianza del proyecto | no (es código del binario) | **sí**, o queda `blocked` | **sí** |
| Cómo se añade | editar las fuentes y recompilar: **no hay comando** | `regulus node init --lang rust` y `regulus node build` | `regulus node init` |

`regulus module install` **no existe**: el CLI solo tiene `module ls|show|shape` y el core no
tiene un método `module.install`. Es el diseño aprobado para el camino estático (decisión
#10: «instalar es recompilar automático», `11-composition.md` §10), **no construido**; hoy
se hace a mano. No hay gestor de paquetes, ni carga de `.so` ni de WASM. `cargo build -p
<crate>` compila **un crate del workspace**, sirve para probar que un módulo estático
compila solo, y **no lo instala**: el binario no lo contiene hasta que se añade su línea a
`modules.rs` y su dependencia a `regulus-cli` y se recompila `regulus-cli`. Un nodo Rust
por stdio, en cambio, se compila aparte y se carga sin volver a enlazar nada: al abrir el
proyecto, o con `project.reload`, el core arranca el programa nuevo.

```bash
regulus node init <proyecto> --id caps --lang rust   # tools/caps/{Cargo.toml,src/main.rs,src/regulus.rs,regulus-module.toml,.gitignore} + [[modules.local]]
regulus project trust <proyecto>                     # compilar ejecuta código del proyecto (build.rs, macros)
regulus node build <proyecto> --id caps              # cargo build --release → tools/caps/target/release/node-caps
regulus project reload <id-del-proyecto> --module caps   # o abrir el proyecto: el core arranca el programa
```

- **Qué escribe `node init --lang rust`:** un crate que es su propio workspace (un
  `[workspace]` vacío: un workspace de Cargo por encima no lo adopta), con `serde_json`
  como única dependencia, un manifiesto con `id = "local/<id>@0.1.0"` (sin él el core lo
  llamaría `<proyecto>/<id>@0.0.0`) y el nodo inicial. Registra la entrada
  `[[modules.local]]` con el mismo escritor que Python. No confía en el proyecto ni
  compila.
- **`regulus node build <carpeta> --id <id>`** exige un proyecto de confianza **antes** de
  ejecutar nada, solo ejecuta `cargo` (no arranca el módulo), respeta `$CARGO` y construye
  siempre en `tools/<id>/target` **ignorando `CARGO_TARGET_DIR`, `CARGO_BUILD_TARGET_DIR` y
  `CARGO_BUILD_TARGET`**, para que el `command` registrado apunte al binario construido.
  Lee de cargo (`--message-format=json-render-diagnostics`) qué ejecutable construyó y
  falla cerrado, sin registrar ni cambiar nada, si no es `target/release/node-<id>`: por
  ejemplo, si un `build.target` de un `.cargo/config.toml` lo deja en `target/<triple>/` (el
  mensaje nombra ese ajuste); el core corre el programa en esta máquina. Avisa si el
  `command` de la entrada no es el construido. Sin construir, el módulo sale `broken` (el
  programa no existe); sin confianza, `blocked`.
- **El ayudante.** El crate generado lleva `src/regulus.rs`, copia del fichero único
  `sdk/rust/regulus.rs` (solo `serde_json` y la biblioteca estándar; el binario `regulus`
  lo lleva dentro, así que `node init` funciona sin el repo) que esconde el protocolo:
  `Module::new(manifiesto).node(tipo, manejador).run()`, con `Context` (`config(id)`,
  `emit(puerto, valor)`, quién es el nodo), `Error::invalid` (`-32602`) y `Error::failed`
  (`-32000`; también cualquier error convertido con `?`). Atiende los eventos en un grupo
  de hilos (4 por defecto, `REGULUS_MAX_WORKERS`): el core entrega de uno en uno los eventos
  de una ejecución, así que el grupo solo paraleliza **entre** ejecuciones y un evento
  lento de una no frena a las demás. **Un `panic` en un manejador falla solo esa llamada**
  (el mensaje va a stderr y el módulo sigue; con `panic = "abort"` no se puede capturar y el
  módulo sale `broken`). Nunca escribe una línea más larga que `max_line_bytes`
  (`REGULUS_MAX_LINE_BYTES`). Stdout y stdin son del protocolo y **el ayudante no los
  redirige** (a diferencia del SDK de Python): se registra con `eprintln!`, y un `print!`
  suelto rompe el siguiente mensaje. Tiene pruebas propias (`sdk/rust/tests.rs`,
  compiladas dentro de `crates/regulus-cli/tests/rust_node.rs`, que corre en `cargo test
  --workspace`: protocolo, códigos de error, pánico, concurrencia entre ejecuciones,
  límites de línea); su referencia es `sdk/rust/README.md` y la lista de lo que no hace está
  al principio de `regulus.rs`. **No hay crate publicado, ni registro de paquetes:** el
  ayudante viaja con cada nodo, como el SDK de Python, y el core no comprueba qué copia
  tiene un nodo.
- **El ejemplo** `examples/local-modules/rust-stdio/` es, byte a byte, lo que escribe
  `node init --lang rust --id rust-stdio` (un test lo vigila).
- **Probado de punta a punta** con un core real (`crates/regulus-cli/tests/rust_node.rs`):
  init → `blocked` → el build se rechaza sin confianza → confiar → `broken` (aún sin
  programa) → build → abrir → `idle` → un perfil devuelve `> HELLO THERE` → cerrar el
  proyecto no deja proceso.
- **Límites:** solo `emit` de `ModuleCtx` y sin ciclo de vida por nodo; el core no aplica
  los defaults de los campos (se leen con su valor por defecto en el manejador); sin sandbox; el build va aparte (un `cargo
  run` en `command` se pasaría de `start_timeout_ms`). Editar el código Rust no está en la
  app: la edición integrada (§4) es solo para el `.py` de los nodos de Python.

Si hace falta todo `ModuleCtx` o va en el camino caliente, el camino es un crate enlazado
(`03-modules.md` §5). Descartados: `.so` (ABI inestable) y WASM (`plan.md`).

## 7. Lo siguiente, y qué de lo propuesto se construyó

El 2026-11-11 Alexander autorizó cerrar para 0.2.1 el editor, el parser, recargar y las
variantes. Esta tabla dice, fase por fase, **qué parte de lo propuesto el 2026-11-10 se
construyó y cuál no**: no está construida la fase entera si falta algo de su lista. Sale de
la investigación del 2026-11-10 (n8n, Node-RED, ComfyUI, Unreal, Blender, Temporal,
LangGraph; fuentes y lecciones en §9).

| Fase | Construido (candidato 0.2.1) | No construido |
|---|---|---|
| F2 · recargar y depurar | ✅ `project.reload` en RPC y CLI (§2.4); ✅ diagnóstico de puertos viejos con la acción «sincronizar» (§2.5); ✅ un proyecto sin confianza sigue `blocked`. **Desvío:** las ejecuciones vivas no «siguen con su copia»: la recarga se niega o las para si se pide | ❌ botón «abrir en editor externo» (`plugin-opener`); ❌ ver el stderr reciente del módulo en Ajustes → Módulos |
| F3 · variantes | ✅ puertos generados desde un campo lista (`repeat`); ✅ puertos y campos condicionales (`when`, el `show_if` propuesto); ✅ una sola `shape(config)` en `regulus-proto` expuesta como `module.shape`: la app, el CLI y la TUI al crear un nodo dan la misma forma; los perfiles antiguos cargan igual (§2.2) | ❌ `capability = "$T"` resuelta por el primer cable; ❌ mínimo y máximo de puertos que crecen; ❌ la TUI usa `shape()` solo al crear un nodo: no la repite al editar un campo ni oculta campos con `when` |
| F4 · editor y parser | ✅ CodeMirror 6 cargado solo al abrir un campo `code` o una fuente; ✅ errores de sintaxis con Lezer; ✅ leer la interfaz de un `run.py` con `ast` **sin ejecutarlo** (§2.3); ✅ primera pantalla <1 s; ✅ versiones en `06-stack.md` | ❌ el aviso de `{{nombre}}`; ❌ **teclear en <16 ms con 1000 líneas: no se cumple en el p90** (§4.3) |
| F5 · nodo Script | — | 🔶 Python dentro del perfil con un runner persistente, puertos por instancia y la misma API del SDK; vetable con `--deny-module`. Sin cambios |
| F6 · más `ModuleCtx` por stdio | ✅ un ayudante de Rust de un fichero (§6), que no añade superficie al core | 🔶 `store`, `checkpoint`, `call_workflow`, ciclo de vida por nodo en el SDK: **conversación de diseño antes**, es superficie nueva del core |

Anotado en la revisión del 2026-11-10, también 🔶 y sin aprobar: que la TUI edite `code` y
`multiline` en varias líneas, y que cada `emit` vaya a la llamada de su ejecución (§5).

**Direcciones de las referencias** (§9), también 🔶, sin aprobar y sin orden entre ellas:
lo que separa un editor de nodos profesional de un lienzo, con lo que ya existe para
reutilizar y el primer paso que se podría aceptar. Ninguna tiene código todavía.

| Dirección | Reutiliza | Siguiente paso y aceptación |
|---|---|---|
| **R1 · Ver valores y depurar paso a paso** (Unreal, LangGraph) | el diario `events` guarda cada `Emitted { value }` con su `seq` y sobrevive al reinicio; Operar y la TUI ya filtran por nodo | *Ver*, solo clientes: en Operar cada puerto enseña el último valor que emitió en la ejecución elegida, o "sin datos"; `regulus events` gana `--node`. App, TUI y CLI dan el mismo valor, también en ejecuciones de antes de reiniciar, sin RPC nuevo y en <16 ms con `event_keep` lleno. *Parar*, core y **conversación de diseño**: un breakpoint retiene la entrega a un nodo, con `continue` y `step` por RPC y CLI, y es del usuario, no del perfil. En `A→B→C` con breakpoint en B se ve el valor de A y B no lo recibe hasta `continue`; `step` entrega uno; un breakpoint en un nodo que no existe sale como aviso; la matriz de ablación pasa |
| **R2 · Puertos genéricos y que crecen** (ComfyUI, Unreal); afina F3 | puertos por nodo en el perfil, el rechazo por capacidad de `canConnect` (`capability`) y de `profile.check` (`wire.capability_mismatch`), `Field.extra` | Además de lo de F3: el template tiene nombre y lo comparten entradas **y** salidas, admite una lista de tipos permitidos, lo que crece tiene mínimo y máximo, y el tipo se guarda resuelto en el perfil. Un `$T` con permitidos rechaza otro tipo como rechazo por capacidad, igual en app, TUI y `profile.check`; un nodo que crece no pasa de su máximo |
| **R3 · Parámetros expuestos de una caja** (Blender, Node-RED) | cajas con puertos `self.*` (`11-composition.md` §6), `workflow.embeds_itself`, y `Node.exposed_parameters`, que existe en `regulus-proto` y ningún cliente rellena | Marcar un campo de un nodo interior como expuesto (id, etiqueta, descripción, defecto) y que el nodo caja lo enseñe como campo propio. El mismo workflow usado dos veces como caja con valores distintos da salidas distintas; guardar y recargar lo conserva; los perfiles antiguos cargan igual; hay comando de CLI. Después, paneles y "ocultar valor", que solo cambian el dibujo |
| **R4 · Pausa duradera en nodos Python** (LangGraph; no el replay de Temporal); parte de F6, **conversación de diseño** | `ModuleCtx::checkpoint`, el `checkpoint` de `NodeStart`, `[store] resume`, como ya hace `human` | `ctx.checkpoint(state)` en el SDK y el estado de vuelta en `node/start`. Un nodo Python guarda estado, se mata el core con `kill -9` y, al arrancar, la ejecución sigue con el mismo id y el nodo recibe su estado; una hija sigue `interrupted`; sin checkpoint nada cambia. El SDK dice que lo anterior al checkpoint puede repetirse ("al menos una vez") y pide claves de idempotencia |

❓ **ABIERTO** (para Alexander): ¿`source.*`, `project.reload`, `node.init` y `profile.sync`
también en el MCP de control? Hoy **no** están: su tabla es fija (ocho herramientas, sin RPC
genérico), así que ningún método nuevo esquiva `--read-only` ni `--deny-module`
(`17-control-mcp.md`). ¿El nodo Script va como tipo de `process` o en un crate propio?
¿«Abrir en el editor externo» y el stderr del módulo en Ajustes (F2) entran en el plan? El
editor integrado se quedó en los campos `code` y en el `.py` de un módulo local: ¿hace falta
algo más, o es un IDE? ¿R1 entra en el plan (ver no toca el core; parar sí)? ¿R3 usa
`exposed_parameters` tal cual o se rediseña ese campo? ¿R4 va con el resto de F6 o por
separado?

## 8. Las seis preguntas

1. *Flexible:* `max_workers` y el tope de línea (`REGULUS_MAX_LINE_BYTES`) son
   configuración; la versión y el nombre son argumentos. `[local_modules] python`,
   `check_timeout_ms` y `max_source_bytes` (§2.6) y los tiempos del editor en la app
   (`codeCommitMs`, `codeLintMs`, `sourceCheckMs`, `shapeDelayMs`, `reloadTimeoutMs`,
   `checkTimeoutMs`, `sourceIndent`, `codeMinLines`, `codeMaxLines`: `04-client.md` §13) son
   configuración, no literales. `tools/` de `node init` es fijo: es dónde genera, y la
   entrada de `regulus.toml` puede apuntar a cualquier otro sitio.
2. *Modular:* es un módulo local; `--without <proyecto>/<id>` y el veto lo quitan.
3. *Rápido:* medido (§2.4, §3, §4.3); dentro del presupuesto de arranque de locales y la
   primera pantalla sigue por debajo de 1 s. Por tecla **no** entra en 16 ms en el p90
   (editor nuevo incluido). Los números están en §4.3; el presupuesto no se ha tocado.
4. *Personalizable:* se cambia editando `run.py` (en la app o fuera) y recargando el módulo,
   sin reiniciar el core.
5. *Infraestructura:* `regulus node init|source|save|check|build`, `project trust|reload`,
   `profile check|sync`, `module ls|show|shape`: cada acción de la app tiene su comando.
6. *Caja líquida:* forma fija (el protocolo y `shape()`); por dentro, Python, Rust o lo que
   hable JSON.

## 9. Referencias y lecciones

Documentación oficial leída el 2026-11-10. Son lecciones, no decisiones: lo que §7 no
construyó y las direcciones R1-R4 siguen 🔶.

| Sistema | Qué hace (fuente oficial) | Lección para Regulus |
|---|---|---|
| Unreal | Wildcards, `DeterminesOutputType`, Add Pin ([UFunctions](https://dev.epicgames.com/documentation/en-us/unreal-engine/ufunctions-in-unreal-engine)); breakpoint antes del nodo, watch con el último valor, "sin datos" si no se ejecutó, breakpoints locales en `.ini` ([Debugger](https://dev.epicgames.com/documentation/en-us/unreal-engine/blueprint-debugger-in-unreal-engine)) | Ver y parar son dos cosas. Ver sale del diario `events`, que ya guarda cada valor; parar es core (R1) |
| ComfyUI V3 | `MatchType` con tipos permitidos, `Autogrow` con mínimo y máximo, `DynamicCombo`, `fingerprint_inputs`, `is_output_node` ([V3](https://docs.comfy.org/custom-nodes/v3_migration)); entradas `lazy` ([lazy](https://docs.comfy.org/custom-nodes/backend/lazy_evaluation)) | El template lo comparten entradas y salidas, con lista de permitidos y límites, y se guarda resuelto (R2) |
| Blender | Group Sockets: tipo, descripción, defecto, ocultar valor, paneles ([manual](https://docs.blender.org/manual/en/latest/interface/controls/nodes/groups.html), [API](https://docs.blender.org/api/current/bpy.types.NodeTreeInterface.html)) | Una caja expone campos, no solo puertos (R3) |
| n8n | Code node por task runners; el modo interno (hijo con el mismo uid) es "insecure by design" y está *deprecated* desde 3.0; allowlists ([Code](https://docs.n8n.io/build/code-in-n8n/using-the-code-node), [runners](https://docs.n8n.io/deploy/host-n8n/configure-n8n/set-up-task-runners)) | Un proceso aparte no es aislamiento: F5 no promete sandbox |
| Node-RED | Subflow sin recursión, una entrada, propiedades por instancia como variables de entorno ([subflows](https://nodered.org/docs/user-guide/editor/workspace/subflows)); Function con On Start/On Stop y timeout solo para código síncrono ([functions](https://nodered.org/docs/user-guide/writing-functions)) | El timeout de F5 mata el proceso; `node/start` y `node/stop` son el On Start y el On Stop; propiedades por instancia (R3) |
| Temporal | Ejecución durable "effectively once" por replay determinista del Event History; lo no determinista va en Activities ([execution](https://docs.temporal.io/workflow-execution), [definition](https://docs.temporal.io/workflow-definition)) | Regulus reanuda por checkpoint, no por replay: no exige determinismo, y el replay no se propone |
| LangGraph | `interrupt` guarda estado y espera; al reanudar, el nodo empieza de nuevo; los efectos previos deben ser idempotentes ([interrupts](https://docs.langchain.com/oss/python/langgraph/interrupts), [checkpointers](https://docs.langchain.com/oss/python/langgraph/checkpointers)) | Si F6 da `checkpoint` por stdio, el SDK documenta "al menos una vez" y claves de idempotencia (R4) |

Vistos y no propuestos: el replay determinista de Temporal (obliga a reescribir los
módulos como código determinista) y reejecutar desde un punto o bifurcar (ejecución
parcial de ComfyUI, *fork* de LangGraph), que exige guardar las entradas de cada nodo:
mejor esperar a que se use R1. El aislamiento sigue fuera de v1 (`11-composition.md` §12).
No consta en lo leído el aislamiento de procesos de ComfyUI, y no se afirma ninguna
posición de mercado.
