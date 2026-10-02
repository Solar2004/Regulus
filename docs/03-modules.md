# 03 — El sistema de módulos

> **v0.3** · 2026-10-02
> El contrato que todo módulo cumple. Lee [`01-vision.md` §2](01-vision.md) (doctrina
> Predator) y [`02-architecture.md` §1](02-architecture.md) (los dos límites) primero.

---

## 1. Capacidades, no nombres

✅ **DECIDIDO** — **Los módulos nunca se conocen por nombre, solo por capacidad.** El
core hace de casamentero: cuando un módulo pide `memory.store`, el core le da quien lo
provea.

Consecuencia: la implementación de memoria se puede reemplazar por otra sin tocar nada
más. Y es lo que hace posible el invariante de ablación (§4).

```toml
# crates/modules/agents/regulus-module.toml
[module]
id      = "agents"
name    = "Agents"
version = "0.1.0"

[module.origin]
derived_from = "hcom"
author       = "aannoo"
repo         = "https://github.com/aannoo/hcom"
license      = "MIT"
honors       = "aannoo"       # la UI lo muestra en la ficha del módulo

provides = ["agent.source", "agent.spawn", "terminal.stream"]

[[consumes]]
capability = "memory.store"
required   = false            # si falta, el módulo arranca sin memoria

[[consumes]]
capability = "agent.identity"
required   = true             # si falta, el módulo se autodesactiva y lo reporta
```

### 1.1 `required` es el campo más importante del manifest

Es lo que hace que quitar un módulo degrade el sistema en vez de romperlo:

- `required = false` → el módulo arranca sin esa capacidad y funciona en modo reducido.
- `required = true` → el módulo **se autodesactiva y reporta por qué**. No revienta, no
  deja el core en estado inconsistente, y la UI puede decir exactamente qué falta.

Regla: `required = true` es la excepción y hay que justificarla. Por defecto, opcional.

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
    /// Resolver una capacidad. None si nadie la provee.
    pub fn resolve(&self, capability: &str) -> Option<CapabilityHandle>;

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
(§5, requisito 2 de [`01-vision.md`](01-vision.md)). El host no cae.

❓ **ABIERTO** — Mecanismo de registro de módulos: a mano, `inventory`, `linkme` o
`abi_stable`. Es BRIEF-3 en [`05-roadmap.md`](05-roadmap.md). Criterios: ergonomía al
añadir un módulo, si permite la ablación de §4, y compatibilidad con `catch_unwind`.

---

## 3. Estados de un módulo

✅ **DECIDIDO** — Derivado del requisito 2 de [`01-vision.md` §5](01-vision.md): el
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
[`01-vision.md` §1.2](01-vision.md) de promesa en hecho verificado.

**Con N módulos, se arranca Regulus N veces, cada vez quitando uno, y se verifica que el
resto carga y responde.**

```
regulus serve --without agents    → memory, roles, families, mcp OK
regulus serve --without memory    → agents arranca sin memoria (consumes opcional) OK
regulus serve --without roles     → families reporta "dependencia requerida ausente",
                                     se autodesactiva, el resto OK
```

Es automatizable en CI. Sin esto, "puedes quitar cosas y todo sigue funcionando" es una
intención, y las intenciones se degradan en silencio.

### 4.1 Las cuatro reglas que la matriz obliga a cumplir

1. **El core nunca importa un módulo por nombre.** Solo resuelve capacidades.
2. **Toda `consumes` declara `required`.** Si falta una requerida, el módulo se
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
2. Escribir `regulus-module.toml` con `provides` y `consumes`
3. Implementar `Module` (cuatro métodos)
4. Registrarlo (una línea — mecanismo pendiente de BRIEF-3)
5. `regulus module ls` lo muestra · `regulus serve --without <id>` sigue arrancando

**Si el paso 3 requiere tocar `regulus-core`, el contrato está mal.** Ese es el criterio
de aceptación de v1 en [`05-roadmap.md`](05-roadmap.md).
