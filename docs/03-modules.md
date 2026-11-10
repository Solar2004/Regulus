# 03 — El sistema de módulos

> **v0.4** · 2026-10-05 · aplica el modelo de composición
> ([`11-composition.md`](11-composition.md)): un módulo trae nodos y su workflow interno.
> El contrato que todo módulo cumple. Lee [`plan.md` §4.1](plan.md) (doctrina
> Predator) y [`02-architecture.md` §1](02-architecture.md) (los dos límites) primero.

---

## 1. Capacidades, no nombres

✅ **DECIDIDO** — **Los módulos nunca se conocen por nombre, solo por capacidad.** Una
capacidad es un **tipo de puerto con versión** (`llm.chat@1`). Un nodo que necesita
`llm.chat@1` recibe lo que esté conectado a esa entrada por un cable "usa", venga del
módulo que venga.

Consecuencia: un proveedor se reemplaza por otro sin tocar nada más. Y es lo que hace
posible el invariante de ablación (§4).

**Un módulo es un paquete que trae nodos** y, si quiere, un workflow interno (§1.2). Su
manifest declara ambos. Ejemplo:

```toml
# crates/modules/agent/regulus-module.toml
[module]
id      = "regulus/agent@0.1.0"     # autor/módulo@versión
name    = "Agent"

# [module.origin] — opcional: si deriva de código ajeno (doctrina Predator), de dónde
# viene, licencia y a quién honra. La UI lo muestra en la ficha del módulo.

[[node]]
type = "agent"
name = "Agent"

  [[node.port]]
  id          = "model"
  capability  = "llm.chat@1"
  direction   = "input"
  kind        = "uses"
  required    = true            # sin modelo, el nodo se desactiva y lo reporta
  # cardinality = "one"         # por defecto; "many" admite varios cables

  [[node.port]]
  id          = "skills"
  capability  = "skill@1"
  direction   = "input"
  kind        = "uses"
  required    = false
  cardinality = "many"

  [[node.field]]                # opciones que la UI dibuja sola (§1.3)
  id      = "name"
  type    = "text"
  default = "agent"
```

**Lo que la base deja abierto a propósito** (`11` §12): `cardinality` existe, por
defecto `one`; y el formato de cada tipo de puerto lo fija el primer módulo que lo use.
Cambiar de formato es subir la versión (`llm.chat@2`), no romper a los demás. Añadir un
campo **opcional** que los antiguos pueden ignorar no cambia la forma y no sube versión
(Alexander, 2026-10-17).

### 1.1 `required` es el campo más importante del manifest

Vale para cada **entrada** de cada nodo.

Es lo que hace que quitar un módulo degrade el sistema en vez de romperlo:

- `required = false` → el módulo arranca sin esa capacidad y funciona en modo reducido.
- `required = true` → el módulo **se autodesactiva y reporta por qué**. No revienta, no
  deja el core en estado inconsistente, y la UI puede decir exactamente qué falta.

Regla: `required = true` es la excepción y hay que justificarla. Por defecto, opcional.

### 1.2 El workflow interno

✅ **DECIDIDO** (`11` §12) — Un módulo puede traer su workflow interno ya armado (p. ej.
Comunicación: recibir → encolar → entregar → reintentar). **Se edita en la interfaz;
Regulus lo genera como Rust y lo compila.**

- **Se ve todo** lo que ejecuta. Sin cajas negras.
- **Se edita solo lo que el módulo expone** (`exposed`).
- 🔶 **Quién recompila:** quien **crea** el módulo, al crearlo o publicarlo. Quien lo
  **usa** solo toca lo expuesto, que se guarda en fichero y se aplica sin recompilar
  (principio Personalizable).

### 1.3 La UI de un módulo

✅ **DECIDIDO** (`11` §12) — **Por defecto, el sistema de Regulus:** el módulo declara
campos (`[[node.field]]`: texto, número, desplegable, mapa `nombre = valor`…) y la UI los dibuja sola. Ningún
módulo necesita escribir UI. Un módulo **puede** traer su propia UI si la necesita; cómo
se enchufa sin que la UI lo conozca por nombre se decide cuando el primero lo pida.

---

## 2. El contrato

✅ **DECIDIDO**

```rust
pub trait Module: Send + Sync {
    fn manifest(&self) -> &Manifest;
    fn init(&mut self, ctx: &ModuleCtx) -> Result<()>;
    fn call(&self, method: &str, params: Value) -> Result<Value>;
    fn shutdown(&mut self) -> Result<()>;
}
```

Cuatro métodos. Nada más. Tres propiedades que esto compra:

1. **`call` usa los mismos nombres de método que el socket externo.** Un módulo no sabe
   si la llamada vino de la GUI, del CLI o de otro módulo.
2. **La puerta a otros lenguajes está abierta.** `ProcessModule` implementa este trait
   y habla por stdio; el protocolo está en §6 (construido 2026-11-08).
3. **Cada módulo es su propio crate** → se testea solo, se razona solo, y se borra
   quitando una línea del registro.

### 2.1 `ModuleCtx` — lo único que un módulo ve del mundo

```rust
impl ModuleCtx {
    /// Lo conectado por cable "usa" a esa entrada de ese nodo, en esa ejecución.
    pub fn resolve(&self, node: &NodeRef, port: &str) -> Vec<Endpoint>;

    /// Llamar al nodo del otro extremo. Le llega como `NodeCall` con su configuración.
    pub fn call(&self, to: &Endpoint, method: &str, args: Value) -> Result<Value>;

    /// Directorio propio de ese nodo, para este módulo y su proyecto. Se conserva entre
    /// ejecuciones. Cuelga de la casa de Regulus (`REGULUS_HOME`), así que se mueve con ella.
    pub fn node_dir(&self, node: &NodeRef) -> Result<PathBuf>;

    /// Pone un valor en la salida "luego" `port` de un nodo suyo. El core lo entrega
    /// como `node/event` a lo cableado (`02-architecture.md` §6). No hay `subscribe`.
    pub fn emit(&self, node: &NodeRef, port: &str, value: Value) -> Result<()>;

    /// Su espacio clave-valor en el store: el de la base del proyecto del nodo, o el
    /// global sin nodo. El namespace es su id; no puede nombrar otro.
    pub fn store(&self, node: Option<&NodeRef>) -> Result<ModuleStore>;

    /// El mismo espacio en la base de `project` sin un nodo en marcha: lo guardado sigue
    /// legible tras acabar las ejecuciones o reiniciar el core.
    pub fn project_store(&self, project: &str) -> Result<ModuleStore>;

    /// Guarda el estado de un nodo suyo. Si el core cae, el siguiente reanuda la
    /// ejecución y se lo devuelve en `node/start` (`02-architecture.md` §6).
    pub fn checkpoint(&self, node: &NodeRef, state: Value) -> Result<()>;

    /// Arranca una ejecución hija de un workflow del mismo perfil con esas entradas y
    /// devuelve su id. Su primera salida y su final le llegan al nodo como `node/child`
    /// (`02-architecture.md` §4 regla 5).
    pub fn call_workflow(&self, node: &NodeRef, workflow: &str,
                         inputs: Vec<(String, Value)>) -> Result<String>;
}
```

🔶 `resolve` recibe el nodo porque un módulo atiende a todos sus nodos de todas las
ejecuciones con una sola instancia. Los métodos de una capacidad (p. ej. los de
`agent.engine@1`) **no** van en `methods`: varios módulos los sirven a la vez y solo se
alcanzan a través de un `Endpoint`, nunca por nombre global.

✅ `node_dir` lo pidió el perfil aislado de los motores (`12-agents.md` §6.3). Es
`<casa>/data/{global | projects/<proyecto>}/<módulo>/<workflow>/<nodo>`: un módulo no puede ver los
directorios de otro módulo ni de otro proyecto. Si el usuario quiere el perfil en otro
sitio, lo dice en un campo del nodo. Es la parte en ficheros del namespace del store
(`02-architecture.md` §6); la parte en base de datos es `store`, que pidieron `human`,
`tasks` y `memory` (`13-blocks.md` §2, 2026-11-07). ✅ `call_workflow` lo pidió `flow`
(`call`) y las plantillas con subagentes (`13-blocks.md` §2 #4, 2026-11-07); no bloquea:
la respuesta viaja por el bus del padre como cualquier evento.

**Si un módulo necesita algo que `ModuleCtx` no da, es una conversación de diseño, no un
`pub` nuevo.** Esta superficie es deliberadamente pequeña: cada método que se le añade
es una forma más de que los módulos se acoplen al core.

### 2.2 Contención de pánicos

El precio de in-process es que un `panic` puede matar el proceso. Se contiene en el
borde:

```rust
// en regulus-core, al invocar un módulo
let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
    module.call(method, params)
}));

match result {
    Ok(r)  => r,
    Err(_) => {
        // el módulo pasa a estado Broken, el resto del sistema sigue
        self.mark_broken(module_id);
        Err(Error::ModulePanicked(module_id))
    }
}
```

Un módulo que entra en pánico pasa a estado `Broken` y la UI lo muestra como tal
(§3, y [`04-client.md` §3](04-client.md)). El host no cae.

🔶 **PROPUESTO** (2026-10-17, lo pide la caja Agente, `12-agents.md`) — **Cómo ejecuta el
core un nodo.** El trait no cambia.

1. `execution.start { profile_id, workflow_id? }` copia el workflow (la ejecución no ve
   cambios posteriores del perfil) y llama a `node/start` en cada nodo de módulo, en
   orden de cables "usa": primero lo que se usa, luego quien lo usa. `execution.stop`
   llama a `node/stop` en orden inverso. Un cable que entra en una entrada `lazy` no
   cuenta para el orden: el nodo solo la lee en marcha, nunca al arrancar, y así puede
   cerrar un ciclo (Alexander, 2026-10-18).
2. `node/start`, `node/stop`, `node/event`, `node/child` (lo que devuelve una hija) y
   `node/hook` (una pregunta de un motor de agente a un nodo `hook@1`, que el core
   reenvía desde `hook.dispatch` y espera como mucho `[hooks] timeout_ms`;
   `13-blocks.md` §4) son nombres reservados: el core los envía al módulo dueño
   del nodo y no se pueden declarar en `methods`. Un módulo que no los implementa
   (`UnknownMethod`) simplemente no tiene ciclo de vida.
3. Toda llamada dirigida a un nodo lleva `NodeCall { execution_id, node, args }`: el nodo
   con su configuración y los argumentos de quien llama.
4. Un nodo con una entrada `required` sin nada utilizable detrás (nada conectado, o lo
   conectado es de un módulo que no está) **no arranca**: queda en `disabled` de la
   ejecución con las entradas que le faltan, y lo mismo quien dependa de él. El resto de
   la ejecución sigue (§1.1).
5. Si un `node/start` falla, la ejecución pasa a `failed` con el motivo y se paran los
   nodos ya arrancados.

Los cables "luego" no ordenan el arranque: lo que un nodo emite al arrancar se entrega
cuando toda la ejecución corre (`02-architecture.md` §6).

✅ **DECIDIDO** (#10, 2026-10-17) — Registro **a mano**: una línea por módulo en
`crates/regulus-cli/src/modules.rs`. Instalar es **recompilar automático** (`11` §10):
`regulus module install X` añade el crate y escribe esa línea. `inventory` y `linkme`
quedan descartados porque pelean con la ablación (§4).

---

## 3. Estados de un módulo

✅ **DECIDIDO** — El
registro muestra estado real, no solo "instalado".

```rust
pub enum ModuleState {
    Active   { since: Timestamp },          // cargado y usado
    Idle     { last_used: Option<Timestamp> }, // cargado, nunca o hace mucho
    Disabled { missing: Vec<String> },      // falta una capacidad required
    Broken   { reason: String },            // panic o fallo en init
    Blocked  { reason: String },            // no arrancado a propósito (§6, proto 1.14)
}
```

`Blocked` lo usa hoy un módulo local de un proyecto sin confianza (§6). No es usable, pero
tampoco está roto: basta con decidir.

`Idle` con `last_used: None` es el estado que importa: **un módulo que instalaste y
nunca usaste.** La UI debe hacerlo visible sin que haya que buscarlo.

---

## 4. La matriz de ablación

✅ **DECIDIDO** — Esto es lo que convierte el invariante de
*quitar cualquier módulo deja el resto funcionando* de promesa en hecho verificado.

**Con N módulos, se arranca Regulus N veces, cada vez quitando uno, y se verifica que el
resto carga y responde.**

```
regulus serve --without communication → agente, proveedores, modelos, procesos OK
regulus serve --without models        → agente sigue con el proveedor conectado OK
regulus serve --without providers     → el nodo agente reporta "entrada requerida
                                         llm.chat@1 sin cable", se desactiva, el resto OK
```

Es automatizable en CI. Sin esto, "puedes quitar cosas y todo sigue funcionando" es una
intención, y las intenciones se degradan en silencio.

### 4.1 Las cuatro reglas que la matriz obliga a cumplir

1. **El core nunca importa un módulo por nombre.** Solo resuelve capacidades.
2. **Toda entrada declara `required`.** Si falta una requerida, el módulo se
   autodesactiva y lo reporta; no revienta el host.
3. **La UI de un módulo ausente muestra "no instalado"**, no un error.
4. **Ningún módulo asume el orden de carga de otro.**

### 4.2 Cuándo se construye

**Junto al segundo módulo, no al final.** Razón: con un solo módulo la matriz es trivial
y no prueba nada; con cinco módulos ya escritos, hacerla pasar es un refactor. Con dos,
cuesta medio día y desde ahí se mantiene sola.

**Precondición para absorber hcom:** la matriz tiene que existir antes. Absorber un
módulo grande sin ella es exactamente cómo se pierde la modularidad sin darse cuenta.

---

## 5. Escribir un módulo

El camino completo, que es lo que la métrica norte mide (objetivo: menos de una hora):

1. `cargo new --lib crates/modules/<id>`
2. Escribir `regulus-module.toml`: sus nodos, sus puertos y sus campos
3. Implementar `Module` (cuatro métodos)
4. Registrarlo: una línea en `crates/regulus-cli/src/modules.rs` (decisión #10)
5. `regulus module ls` lo muestra · `regulus serve --without <id>` sigue arrancando

**Si el paso 3 requiere tocar `regulus-core`, el contrato está mal.** Ese es el criterio
de aceptación de v1.

---

## 6. Módulos fuera de proceso: el protocolo

✅ **DECIDIDO** (2026-11-08, lo pidió `[[modules.local]]`, `11-composition.md` §13) — Un
módulo puede ser **un proceso aparte en cualquier lenguaje**. El core lo arranca, le
habla **JSON-RPC 2.0 por stdin/stdout, un objeto por línea**, y lo envuelve en
`ProcessModule` (`regulus-core/src/process.rs`), que implementa el mismo trait `Module`.
Para el resto del core es un módulo más.

**Por qué esta forma:** el protocolo **es el trait, método a método**. No hay un
segundo contrato que mantener: lo que valga dentro de proceso vale fuera, con los
mismos nombres de método (§2, propiedad 1). JSON por líneas es lo que cualquier lenguaje
lee con su biblioteca estándar. El ejemplo en Python (`examples/local-modules/echo/`)
ocupa menos de cien líneas. Se descartó un protocolo "simplificado" (nodo, puerto y
valor → salidas) porque cerraba la puerta al ciclo de vida de los nodos, a `node/hook` y
a `node/child`.

| Dirección | Mensaje | Equivale a | Qué lleva |
|---|---|---|---|
| core → proceso | `manifest` (petición) | `Module::manifest` | `params { protocol, project_id, id }`. Responde el manifiesto con la forma de `regulus-module.toml`: un objeto JSON o **el texto TOML** del fichero. `module.id` y `module.name` pueden faltar |
| core → proceso | `start` (petición) | `Module::init` | `{ protocol, module_id, project_id, root }`. Opcional: si responde `-32601`, arranca igual |
| core → proceso | `call` (petición) | `Module::call` | `{ method, params }`, con el método y los parámetros tal cual: `node/start`, `node/event`, `node/stop`, `node/child` y `node/hook` con su `NodeCall` (§2.2). Errores: `-32601` → `UnknownMethod` (un nodo sin ciclo de vida), `-32602` → `InvalidParams`, otro → `Failed` |
| core → proceso | `stop` (petición) | `Module::shutdown` | `{}`. Opcional. Después se cierra su stdin, y llega SIGTERM al grupo y SIGKILL si pasa el margen |
| proceso → core | `emit` (notificación) | `ModuleCtx::emit` | `{ execution_id, node_id, port, value }`. Lo emitido mientras atiende una `call` se publica en el hilo que llama, **antes** de que la llamada vuelva, y sigue su cadena: `[bus] max_depth` corta también los bucles que pasan por el proceso |
| proceso → core | otra petición | — | se responde `-32601`. El resto de `ModuleCtx` (`resolve`, `call`, `store`, `checkpoint`, `node_dir`, `call_workflow`) **no se ofrece todavía**: se añade cuando un módulo local lo pida |

Una línea por mensaje, como viajan (`→` del core al proceso, `←` del proceso al core):

```
→ {"jsonrpc":"2.0","id":1,"method":"manifest","params":{"protocol":"1.14","project_id":"demo","id":"echo"}}
← {"jsonrpc":"2.0","id":1,"result":"[module]\nname = \"Echo\"\n[[node]]\ntype = \"echo\"\n…"}
→ {"jsonrpc":"2.0","id":2,"method":"start","params":{"protocol":"1.14","module_id":"demo/echo@0.1.0","project_id":"demo","root":"/home/a/demo"}}
← {"jsonrpc":"2.0","id":2,"result":null}
→ {"jsonrpc":"2.0","id":3,"method":"call","params":{"method":"node/event","params":{"execution_id":"exec-1","node":{"id":"e","type":"echo",…},"args":{"port":"in","capability":"text@1","value":"hola",…}}}}
← {"jsonrpc":"2.0","method":"emit","params":{"execution_id":"exec-1","node_id":"e","port":"out","value":"hola"}}
← {"jsonrpc":"2.0","id":3,"result":null}
→ {"jsonrpc":"2.0","id":4,"method":"call","params":{"method":"node/start","params":{…}}}
← {"jsonrpc":"2.0","id":4,"error":{"code":-32601,"message":"unknown method `node/start`"}}
```

El manifiesto pasa por el mismo código que uno enlazado: el mismo `Manifest`, la misma
comprobación de `requires_proto` y la misma de id repetido. **El id lo pone el core:**
`<proyecto>/<id>@<versión>`, con la versión de `regulus.toml`, si no la que el manifiesto
dice tras `@`, y si no `0.0.0`. Así el veto (`disable = ["<proyecto>/<id>"]`), `--without`
y los perfiles (`module_id = "<proyecto>/<id>"`) lo nombran como a cualquiera.

**Confianza.** ✅ (2026-11-08, revisión) — **Abrir una carpeta nunca ejecuta código.**
Los módulos locales de un proyecto solo arrancan si su carpeta está en `[projects]
trusted` de `config.toml`. Se añade con `regulus project open <ruta> --trust` o con
`regulus project trust <ruta>`, y se quita con `regulus project trust --remove <ruta>`
(`project.trust { path, remove? }` y `project.open { path, trust? }`, protocolo 1.14). Sin
confianza, el proyecto se abre igual y cada módulo local queda **`blocked`** con "untrusted
project: reopen with --trust", sin proceso. Si la confianza cambia con el proyecto abierto,
sus módulos locales arrancan o se paran en ese momento. El core relee la lista del
fichero cada vez: un solo sitio por dato. Desde el protocolo **1.15** (2026-11-08) cada
entrada local de `module.list` lleva `project` (el id de su proyecto) y `regulus module ls`
lo imprime como `project=<id>`: con eso Ajustes → Módulos ofrece confiar en el proyecto
(`04-client.md` §10) sin que el core conozca al cliente.

**El proceso no hereda el entorno del core:** se vacía (`env_clear`) y solo pasan las
variables de `env_passthrough` (por defecto `PATH`, `HOME`, `LANG` y `TERM`) más el `env`
de su entrada en `regulus.toml`. El token del core y los secretos nunca llegan.

**Estados (§3).** Un proceso que no arranca, que no responde `manifest` en
`start_timeout_ms` o cuyo manifiesto no es válido queda **`broken`** con la razón y las
últimas líneas de su stderr, y se mata. Uno que muere después pasa a `broken` igual. El
core y los demás módulos no se enteran. Su stderr va al log del core con el prefijo
`local module <id>:`.

**Antes de lanzar nada** se descartan, sin proceso: una entrada sin `id` o sin `command`,
con un `id` que no es un nombre simple o con un `cwd` absoluto o con `..` (avisos de
`regulus.toml`), y un **id repetido**, dentro de `[[modules.local]]` o con un módulo ya
cargado, que queda `broken` con "duplicate id".

**Líneas y señales.** Cada línea se lee con un tope, `max_line_bytes` (16 MiB), y lo que no
es UTF-8 se lee igual (sustituido). Pasarse en stdout mata el proceso y lo deja `broken`
con la razón. En stderr la línea se corta y se sigue leyendo. Al parar, las señales van
al **grupo** del proceso (`killpg`), también si el líder ya salió, para no dejar huérfanos
a sus hijos. En Linux el hijo recibe SIGTERM si el core muere sin pararlo
(`PR_SET_PDEATHSIG`). Por eso todos los procesos se lanzan desde un hilo que vive tanto
como el core: esa señal salta cuando muere el *hilo* que lanzó el proceso. `regulus serve`
trata SIGTERM como Ctrl-C: se para limpio y para a sus módulos locales.

**Límites, a propósito:**
- Un módulo local **no puede declarar `methods`**: los métodos del socket se registran al
  arrancar el core. Se alcanza por sus nodos. Si lo declara, queda `broken` con la razón.
- Se arranca al abrir el proyecto (o al arrancar el core con el proyecto abierto). Los de
  un proyecto se arrancan **en paralelo**: el core espera al más lento, no la suma. Con el
  ejemplo en Python el core pasó de 37 ms a 80 ms (build debug, 2026-11-08), dentro del
  presupuesto de 200 ms.
- Cambiar `[[modules.local]]` con el proyecto abierto exige cerrarlo y volver a abrirlo.
- Mientras su proyecto está abierto, lo puede usar cualquier perfil, no solo los del
  proyecto. Restringirlo sería cerrar una puerta que nadie pidió cerrar.
- Una ejecución que lo usa cuando el proyecto se cierra sigue con su copia del workflow.
  Las llamadas a ese nodo fallan a la vista en el bus.

**Configuración** (`config.toml`, sección `[local_modules]`):

| Clave | Por defecto | Qué |
|---|---|---|
| `start_timeout_ms` | `10000` | lo que tiene para responder `manifest` y `start` |
| `call_timeout_ms` | `600000` | lo que tiene para cada `call`; `0` = sin límite. **Una llamada colgada retiene el bus de esa ejecución** (sus entregas van en orden) hasta que vence, pero no el core ni las demás ejecuciones |
| `stop_grace_ms` | `2000` | lo que tiene para responder `stop` y luego para acabar tras SIGTERM |
| `stderr_lines` | `20` | líneas de stderr que se guardan para decir por qué se rompió |
| `max_line_bytes` | `16777216` | la línea más larga que puede escribir (16 MiB) |
| `env_passthrough` | `["PATH", "HOME", "LANG", "TERM"]` | las variables del entorno del core que le llegan |

Ablación: la matriz (§4) tiene una fila para el módulo local. Quitar cualquier módulo
enlazado lo deja usable, y quitarlo a él (`--without <proyecto>/<id>`) deja el resto igual
y no arranca ningún proceso, comprobado buscando el proceso por su entorno
(`crates/regulus-cli/tests/ablation.rs`).
