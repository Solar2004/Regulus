# 18 — Nodos propios: escribir nodos en Python (y en Rust por stdio)

> **v0.1** · 2026-11-10 · Primer incremento autorizado por Alexander (nodos propios, SDK
> de Python, edición integrada, Rust si conviene). ✅ *Construido y probado* con procesos
> reales, contra un core real y, el inspector, en un navegador real (§4); pasó una revisión
> independiente y sus defectos están arreglados con test. Lo que sigue al incremento (§7)
> es 🔶 y **no está aprobado**. **No se ha usado aún en un proyecto real.**
> Código: `sdk/python/regulus.py` (SDK), `examples/local-modules/python-sdk/` (ejemplo),
> `crates/regulus-cli/src/node.rs` y `crates/regulus-cli/assets/node/run.py`
> (`regulus node init`), inspector en `crates/regulus-tauri/frontend/src/composition/editor/`.
> Pruebas: `sdk/python/tests/test_regulus.py` (en la CI), `crates/regulus-cli/tests/node_init.rs`.

---

## 1. Qué es

Un **nodo propio** es un tipo de nodo que escribe el usuario en su proyecto, sin tocar el
core ni recompilar. No hay runtime nuevo: es un **módulo local** (`[[modules.local]]`,
`03-modules.md` §6) hecho con un SDK que esconde el protocolo. El core lo trata como a
cualquier módulo: se veta, se quita con `--without` y la matriz de ablación lo cubre.

```
 regulus node init ──▶ tools/<id>/run.py + regulus.py ──▶ [[modules.local]] en regulus.toml
                                                                 │ proyecto de confianza
 inspector (campo code) ──profile.save──▶ core ──JSON-RPC stdio──▶ python3 run.py
                                          ▲  manifest · start · call · stop │
                                          └────────── emit ◀────────────────┘
```

Lo que **no** es: ni un sandbox, ni un IDE, ni un mecanismo que sustituya a los módulos en
Rust (los módulos de serie siguen siendo crates, `plan.md`).

## 2. Escribir un nodo

```bash
regulus node init <carpeta-del-proyecto> --id caps   # tools/caps/run.py + regulus.py
regulus project open <carpeta> --trust               # o: regulus project trust <carpeta>
regulus module ls                                    # <proyecto>/caps@0.1.0   idle
```

`regulus node init <carpeta> --id <id> [--python <programa>]` no necesita el core. Pide
un proyecto que ya exista (`regulus.toml`), un id simple (`[a-z0-9][a-z0-9_-]{0,63}`) y que
`tools/<id>` no exista: **nunca sobrescribe**. Escribe `tools/<id>/run.py` (un nodo de
ejemplo `in → out` con un campo) y la copia del SDK, y añade la entrada
`[[modules.local]]` a `regulus.toml` como texto, para que sigan los comentarios y las
claves que no conoce; si eso no da exactamente la tabla de antes más la entrada, reescribe
la tabla (con aviso: se pierden los comentarios). Si algo falla, borra lo que creó.
**Nunca confía en el proyecto:** imprime el comando para hacerlo, y el `module_id` y el
`type` que lleva un nodo en un perfil.

```python
import regulus as rg

@rg.node("template", name="Template",
         inputs=[rg.then_in("in", "text@1")],
         outputs=[rg.then_out("out", "text@1")],
         fields=[rg.field("pattern", "code", default="{value}", language="python")])
def template(ctx, port, value):
    ctx.emit("out", ctx.config["pattern"].format(value=value))

rg.run(version="0.1.0")
```

| Pieza | Qué hace |
|---|---|
| `node(type, name=, inputs=, outputs=, fields=)` | decorador: declara un tipo de nodo y su manejador. Errores de declaración (id vacío o repetido, puerto en la lista equivocada, valor que no es JSON) saltan **al importar**: el proceso muere y el módulo queda `broken` con su stderr |
| `then_in(id, capability, name=, required=, cardinality="many", lazy=)` | entrada "luego". `many` por defecto: varios cables la alimentan |
| `then_out(id, capability, name=)` | salida "luego" |
| `field(id, type="text", default=, options=, **extra)` | campo del nodo. `type` es abierto (`text`, `number`, `select`, `code`…). Las claves de `extra` (`label`, `description`, `multiline`, `language`…) viajan tal cual en el manifiesto |
| `handler(ctx, port, value)` | se llama por cada valor que llega. Lo que devuelve se ignora (avisa una vez por stderr) |
| `ctx.config` | la configuración del nodo sobre los `default` de sus campos (el core no aplica los defaults; lo hace el SDK, con una copia por evento) |
| `ctx.emit(port, value)` | pone un valor en una salida. Puerto no declarado, valor no JSON, texto que no es UTF-8 (un surrogate suelto de `os.fsdecode`) o línea más larga de lo que lee el core → `ValueError`, y no se envía nada |
| `ctx.execution_id` · `node_id` · `node_type` · `node` · `event` · `project_id` · `root` · `log()` | quién es, el `NodeEvent` crudo, el proyecto (de `start`) y una línea al log |
| `InvalidInput` / `NodeError` | errores a propósito: `-32602` (`InvalidParams`) y `-32000` (`Failed`) |
| `run(name=, version=, max_workers=)` | sirve hasta `stop` o hasta que se cierra stdin |

Copiar el SDK junto a cada `run.py` (lo hace `node init`) evita `PYTHONPATH` y deja el
proyecto autocontenido: cada proyecto fija la versión del SDK con la que se escribió.

## 3. Cómo cumple el protocolo

Verificado contra `crates/regulus-core/src/process.rs` y el ejemplo `echo`:

| Mensaje | El SDK |
|---|---|
| `manifest` | responde un objeto JSON con la forma de `regulus-module.toml`: `requires_proto = "^1"`, `name` si se da, y `id = "local/sdk@<version>"` solo para que el core lea la versión (el id real lo pone el core: `<proyecto>/<id>@<versión>`) |
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

## 4. Campos de código en el inspector

El manifiesto conserva las claves desconocidas de un campo (`Field.extra`, `serde(flatten)`
en `crates/regulus-proto/src/manifest.rs`): salen hacia los clientes planas, tal como las
escribió el módulo, y el core no las lee. Un campo `type = "code"` se dibuja como un
`<textarea>` monoespaciado, y uno `type = "text"` con `multiline = true` como un área de
texto. **Es un área de texto, no un editor:** sin resaltado, sin comprobar sintaxis, sin
autocompletar; debajo de cada campo `code` lo dice un aviso. `language` viaja como pista;
nada analiza el código. Un cliente antiguo que no conoce el tipo lo muestra con el control
`raw`.

| Pista del campo | Qué hace en la app |
|---|---|
| `label` · `description` | nombre visible junto al id, y una línea de ayuda debajo |
| `language` | se ve junto al tipo (`code · python`) |
| `indent` | lo que mete Tab en un `code`: `"tab"` o de 1 a 8 espacios (4 por defecto) |
| `multiline` | solo en `type = "text"`: área de texto que ajusta líneas; Tab cambia de foco |

En un `code`, Tab sangra (cada línea si la selección abarca varias) y Shift+Tab quita
sangría; **Esc y luego Tab sale del campo**, para que el teclado no quede atrapado. El
área crece de 4 a 18 líneas y luego se desplaza (`multiline`: de 3 a 10).

**Probado en un navegador real** (2026-11-10, Chromium con el inspector de verdad contra un
core real, 36 comprobaciones): escribir con saltos de línea, Tab con el cursor donde
debe, Tab y Shift+Tab sobre varias líneas, Esc → Tab saca el foco sin tocar el valor,
`multiline` con Tab que cambia de foco, y los dos valores sobreviven a Guardar
(`Ctrl+S` → `profile.save`) y a recargar la página; el valor guardado llega al módulo tal
cual. **Medido** (build de producción, 30 muestras, de la tecla al siguiente cuadro, así
que incluye esperar a ese cuadro). **Ni la base ni el área de texto entran en los 16 ms**
de `00-principles.md` §3, que siguen como están:

- **Base, de antes de este incremento: ~19 ms de mediana** en el campo de una línea que ya
  existía. Unos 10 ms son el documento del perfil, que se rehace en cada tecla en todos
  los campos del inspector.
- **Área de texto con 2 líneas: ~19-20 ms**, lo mismo que la base: con valores pequeños no
  añade coste.
- **Área de texto con 1000 líneas: ~29-30 ms de mediana y ~40-43 ms el p90.** Aquí sí
  añade coste. Para valores grandes, el camino es F4 (§7) o un editor externo.

Límites del área de texto: Ctrl+Z del navegador **no deshace un Tab** (lo mete el código,
no el navegador; medido); cada tecla es un cambio del perfil, como en los demás campos.
**La TUI no lo dibuja todavía:** un `code` sale como texto crudo de una línea y
`multiline` se ignora; el texto con saltos se edita en el fichero del perfil o desde la app.

Ojo: un `{{nombre}}` dentro del código de un campo es un hueco de plantilla para
`template.apply` y `template.save` (`11-composition.md` §7). Si no está declarado, falla.

## 5. Límites, dichos sin adornos

- **No hay sandbox.** El proceso corre con los permisos del usuario. Lo que protege es la
  confianza en el proyecto (`03-modules.md` §6) y el entorno vacío (`env_passthrough`).
  El aislamiento de ejecución sigue fuera de v1 (`11-composition.md` §12).
- **Del `ModuleCtx` solo hay `emit`.** Sin `store`, `checkpoint`, `node_dir`, `resolve`,
  `call` ni `call_workflow`; sin ciclo de vida por nodo. Añadirlos es conversación de
  diseño (§7, F6).
- **Solo puertos "luego".** Un nodo Python no sirve ni usa capacidades por cable "usa".
- **Recargar exige cerrar y abrir el proyecto** (o reiniciar el core). Los nodos ya puestos
  en un perfil guardan sus puertos: si el manifiesto cambia, no se enteran.
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

## 6. Rust por stdio

El mismo protocolo vale para cualquier binario: solo cruzan líneas JSON. Un programa Rust
que lee stdin con `serde_json` y escribe `emit` y respuestas por stdout ya es un módulo
local (`command = ["tools/x/target/release/x"]`), sin DLL ni ABI y sin tocar el core. Si
comparte tipos con `regulus-proto`, depende del código fuente; `requires_proto` protege la
versión. Un `panic` muere en su proceso.

Costes: serializar cada evento, un proceso por módulo, solo `emit` de `ModuleCtx`, y el
build va aparte (un `cargo run` en `command` puede pasarse de `start_timeout_ms`). Si hace
falta todo `ModuleCtx` o va en el camino caliente, el camino es un crate enlazado
(`03-modules.md` §5). Descartados: `.so` (ABI inestable) y WASM (`plan.md`). **No hay SDK
de Rust todavía** (§7, F6).

## 7. Lo siguiente

🔶 **PROPUESTO**, nada aprobado. Sale de la investigación del 2026-11-10 (n8n, Node-RED,
ComfyUI, Unreal, Blender, Temporal, LangGraph; fuentes y lecciones en §9).

| Fase | Qué | Aceptación |
|---|---|---|
| F2 · recargar y depurar | `project.reload` en RPC y CLI; diagnóstico de puertos viejos con acción "sincronizar"; botón "abrir en editor externo" (`plugin-opener`); ver el stderr reciente del módulo en Ajustes → Módulos | se edita `run.py`, se recarga y el puerto nuevo aparece sin reiniciar el core; un proyecto sin confianza sigue `blocked`; las ejecuciones vivas siguen con su copia |
| F3 · variantes | puertos con `capability = "$T"` resuelta por el primer cable; puertos generados desde un campo lista; `show_if`; una sola función `shape(config)` en `regulus-proto` expuesta como `module.shape` | app, TUI y CLI dan la misma forma; los perfiles antiguos cargan igual |
| F4 · editor y parser | CodeMirror 6 cargado solo al abrir un campo `code`, errores de sintaxis con Lezer, aviso de `{{nombre}}`; leer la interfaz de un `run.py` con `ast` de Python **sin ejecutarlo** | teclear en <16 ms con 1000 líneas; primera pantalla <1 s; versiones en `06-stack.md` |
| F5 · nodo Script | Python dentro del perfil con un runner persistente, puertos por instancia y la misma API del SDK | 1000 eventos medidos; el runner caído se reinicia; el timeout mata el grupo; vetable con `--deny-module` |
| F6 · más `ModuleCtx` por stdio | `store`, `checkpoint`, `call_workflow` con la misma forma que en Rust, ciclo de vida por nodo en el SDK, SDK de Rust | **conversación de diseño antes**: es superficie nueva del core |

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

❓ **ABIERTO** (para Alexander): ¿`project.reload` también en el MCP de control? ¿El nodo
Script va como tipo de `process` o en un crate propio? ¿CodeMirror solo para campos más
editor externo para ficheros, o también ficheros dentro de la app? ¿R1 entra en el plan
(ver no toca el core; parar sí)? ¿R3 usa `exposed_parameters` tal cual o se rediseña ese
campo? ¿R4 va con el resto de F6 o por separado?

## 8. Las seis preguntas

1. *Flexible:* `max_workers` y el tope de línea (`REGULUS_MAX_LINE_BYTES`) son
   configuración; la versión y el nombre son argumentos. `tools/` de `node init` es fijo:
   es dónde genera, y la entrada de `regulus.toml` puede apuntar a cualquier otro sitio.
2. *Modular:* es un módulo local; `--without <proyecto>/<id>` y el veto lo quitan.
3. *Rápido:* medido (§3, §4); dentro del presupuesto de arranque de locales. Por tecla
   **no** entra en 16 ms: ni la base de los campos del inspector, que es de antes, ni el
   área de texto, que con 1000 líneas suma bastante más. Los números están en §4; el
   presupuesto no se ha tocado.
4. *Personalizable:* se cambia editando `run.py` y reabriendo el proyecto.
5. *Infraestructura:* `regulus node init`, `project trust`, `module ls`.
6. *Caja líquida:* forma fija (el protocolo); por dentro, Python, Rust o lo que hable JSON.

## 9. Referencias y lecciones

Documentación oficial leída el 2026-11-10. Son lecciones, no decisiones: lo que propone §7
sigue 🔶.

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
