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
Cambiar de formato es subir la versión (`llm.chat@2`), no romper a los demás.

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
campos (`[[node.field]]`: texto, número, desplegable…) y la UI los dibuja sola. Ningún
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
2. **La puerta a otros lenguajes queda abierta sin coste hoy.** Un `ProcessModule` que
   implemente este trait y hable por stdio encaja sin que el core cambie.
3. **Cada módulo es su propio crate** → se testea solo, se razona solo, y se borra
   quitando una línea del registro.

### 2.1 `ModuleCtx` — lo único que un módulo ve del mundo

```rust
impl ModuleCtx {
    /// Lo conectado por cable "usa" a esa entrada. None si no hay cable.
    pub fn resolve(&self, port: &str) -> Option<CapabilityHandle>;

    /// Publicar en el bus. Los suscriptores son desconocidos para quien publica.
    pub fn emit(&self, event: Event);

    /// Suscribirse a eventos por patrón.
    pub fn subscribe(&self, pattern: &str) -> EventStream;

    /// El namespace propio del módulo en el store.
    pub fn store(&self) -> StoreHandle;
}
```

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

❓ **ABIERTO** — **Cómo ejecuta el core un nodo**: instanciarlo por ejecución, entregarle
los eventos de sus cables "luego". Se concreta al escribir la base (fase E',
[`plan.md` §3](plan.md)). Si exige cambiar el trait, se cambia **aquí
primero**.

❓ **ABIERTO** — Mecanismo de registro de módulos: a mano, `inventory`, `linkme` o
`abi_stable`. Se cierra en el paso 3 de [`plan.md`](plan.md). Criterios: ergonomía al
añadir un módulo, si permite la ablación de §4, y compatibilidad con `catch_unwind`.
Instalar es **recompilar automático** (`11` §10): `regulus module install X` añade el
crate y recompila. Encaja con el registro a mano: la línea la escribe el comando.

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
}
```

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
