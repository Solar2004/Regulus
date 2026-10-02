# IDEA — Sistema de cajas

> **Estado: ❓ SIN DEFINIR.** Decisión #11 de [`../05-roadmap.md`](../05-roadmap.md).
> Este documento **no** es un diseño. Es el sitio donde vive la idea mientras se define.
> **No implementes nada de aquí.**

---

## 1. La idea, en palabras de Alexander

> "Talvez quiero hacer un sistema de cajas en el que pueda darle a módulos capacidades
> o digamos flags o lo que sea, porque como está enfocado en AI, un sistema de proxies o
> lo que se me salga."

Dos cosas mencionadas juntas: **cajas** (dar capacidades/flags a módulos) y **proxies**.
Puede que sean la misma idea o dos distintas. Eso es parte de lo que hay que resolver.

---

## 2. Por qué esto bloquea algo

`Isolation` en [`../02-architecture.md` §4](../02-architecture.md) es un struct fijo:

```rust
pub struct Isolation {
    pub worktree: Option<PathBuf>,
    pub mcp:      McpScope,
    pub memory:   MemoryScope,
    pub env:      EnvScope,
}
```

**Si las cajas se adoptan, `Isolation` probablemente deja de existir** y pasa a ser un
caso particular de caja: "la caja que envuelve a un agente y le controla worktree, MCP,
memoria y entorno".

Por eso `Isolation` no se implementa hasta resolver esto. Hacerlo al revés significaría
escribirlo dos veces.

---

## 3. Lecturas posibles

🔶 **NO DECIDIDO** — Tres interpretaciones de "caja". Alexander elige, corrige, o dice
que es otra cosa. **No son excluyentes necesariamente.**

### Lectura A — Caja = envoltorio (middleware)

Una caja **envuelve** a un módulo e intercepta sus llamadas. Puede añadir, quitar o
modificar comportamiento sin que el módulo lo sepa.

```
  llamada ──▶ [ caja: log ] ──▶ [ caja: rate-limit ] ──▶ [ módulo agents ]
```

- **Precedente:** `tower::Layer` en Rust, middleware de Express, decoradores.
- **Encaja con:** "un sistema de proxies" — un proxy ES un envoltorio.
- **Da:** composición. Apilas cajas y el comportamiento se suma.
- **Cuesta:** el trait `Module` necesita que las cajas sean también `Module` (envoltorio
  transparente). Es elegante pero hay que diseñarlo bien.

### Lectura B — Caja = contenedor (aislamiento)

Una caja es un **recinto** donde algo corre con recursos y permisos definidos. Lo que
está dentro no ve fuera salvo lo que la caja permita.

```
  ┌─ caja "arch" ────────────┐   ┌─ caja "impl" ───────────┐
  │ agente · mcp :3101       │   │ agente · mcp :3102      │
  │ worktree: /wt/arch       │   │ worktree: /wt/impl      │
  │ memoria: proyecto        │   │ memoria: global         │
  └──────────────────────────┘   └─────────────────────────┘
```

- **Precedente:** contenedores, sandboxes, cgroups.
- **Encaja con:** `Isolation` tal cual — sería su generalización directa.
- **Da:** el caso de uso UC-4 (MCP aislado por agente) sale gratis.
- **Cuesta:** menos composable que A. Una caja dentro de otra tiene semántica confusa.

### Lectura C — Caja = preset de configuración

Una caja es un **paquete de flags y capacidades con nombre** que se aplica a un módulo o
a un agente.

```toml
[box.strict]
flags        = ["no-network", "read-only-fs"]
capabilities = ["memory.store"]          # solo esta, nada más
proxy        = "local-only"

[box.research]
flags        = ["allow-network"]
capabilities = ["memory.store", "web.fetch", "agent.spawn"]
proxy        = "openrouter"
```

- **Precedente:** perfiles de AWS, profiles de Cargo, presets de ESLint.
- **Encaja con:** "darle a módulos capacidades o digamos flags".
- **Da:** lo más simple de implementar con mucha diferencia. Es declarativo, sin runtime.
- **Cuesta:** no intercepta nada. No sirve para proxies de verdad.

---

## 4. Lo de los proxies

> "un sistema de proxies o lo que se me salga"

**Los proxies son un ejemplo, no un plan.** Aclarado por Alexander: *"simplemente me
refiero a que es un ejemplo, no usaremos headroom ni token-proxy ni nada de eso"*.

Lo que la frase aporta al diseño no es "hay que construir un módulo proxy". Es el
requisito de fondo:

> **Las cajas tienen que servir para cosas que todavía no existen** — incluida la que a
> Alexander se le ocurra un martes. El diseño no puede asumir un catálogo cerrado de
> tipos de caja.

Consecuencia concreta para quien resuelva esto: si el diseño de cajas necesita una
enumeración de tipos (`enum BoxKind { Isolation, Proxy, RateLimit, ... }`), está mal.
Tiene que ser abierto por construcción.

**No se documenta ningún módulo proxy.** Si algún día aparece, se diseña entonces.

---

## 5. Preguntas que hay que responderle a Alexander

Son las que BRIEF-5 de [`../05-roadmap.md`](../05-roadmap.md) tiene que resolver. **Una a
una, sin inventar respuestas.**

1. **¿Una caja envuelve, contiene, o configura?** (lecturas A / B / C de §3)
2. **¿A qué se le pone una caja: a un módulo, a un agente, o a los dos?** `Isolation`
   hoy es por agente; "darle a módulos capacidades" suena a por módulo.
3. **¿Las cajas se anidan?** Si sí, qué gana la de dentro y qué la de fuera.
4. **El proxy: ¿de LLM o de red?** (§4)
5. **¿Una caja puede quitar una capacidad que el módulo ya tiene,** o solo añadir? Quitar
   es mucho más potente y mucho más difícil.

---

## 6. Lo único que ya se puede afirmar

Cualquier forma que tomen las cajas tiene que respetar el invariante de
[`../01-vision.md` §1.2](../01-vision.md):

**quitar una caja deja el módulo funcionando.** Una caja es decoración sobre un módulo,
nunca un requisito para que arranque. Si un módulo solo funciona dentro de su caja, la
caja es parte del módulo y no debería ser una caja.
