# HANDOFF — Estado actual y cómo continuar

> **v1** · 2026-10-02
> Para un agente nuevo, o para mí sin memoria de la conversación. Contiene lo que **no
> está** en los otros documentos: el razonamiento, lo que se descartó, y qué toca ahora.
>
> **Lee esto después de [`../AGENTS.md`](../AGENTS.md) y de
> [`00-principles.md`](00-principles.md).** Con los tres tienes todo el contexto.

---

## 1. Dónde estamos, en una frase

Toda la fase de diseño está cerrada. **20 decisiones tomadas, 2 abiertas, ninguna
bloquea empezar a construir.** No hay código de aplicación: lo único que existe es
documentación y el branding.

---

## 2. TODO actual, en orden

El orden importa y la razón está en la última columna.

| # | Tarea | Est. | Por qué va aquí |
|---|---|---|---|
| **1** | **Commit inicial.** El repo es git (`master`) **sin ningún commit**. Todo el trabajo existe sin red de seguridad. | 5 min | Es lo único irreversible si algo se borra |
| **2** | **Fase A — mock server.** `jsonrpsee` en `127.0.0.1`, `hello` + los 8 métodos, devolviendo los payloads de [`04-client.md` §12](04-client.md) | medio día | Desbloquea todo el trabajo de UI. Y su lista de métodos **es** la spec del core |
| **3** | **Fase B — UI vista OPERAR** contra el mock, solo lectura | 1 sem | El mockup y los payloads ya existen; es ejecución, no diseño |
| **4** | **Fase C — vista TOPOLOGÍA** + los 4 estados vacíos | 1 sem | Depende de que OPERAR exista para reutilizar el modelo de estado |
| **5** | **BRIEF-6 — spike de ACP** con un agente real | medio día | Independiente. Resuelve la decisión #12, que afecta a todo v2 |
| **6** | **BRIEF-5 — entrevista sobre las cajas** | 1 h con Alexander | Desbloquea `Isolation`, que está congelado |
| **7** | **Fase D — `regulus-proto`** extraído del mock | 1 día | Los tipos ya existirán de facto en el mock |
| **8** | **Fase E — `regulus-core` + `regulus-cli`** | 2-3 días | Implementa los 8 métodos de verdad |
| **9** | **Fase F — módulo `hello` + matriz de ablación** | medio día | Cierra v1 |

**Escribe `hello` antes que cualquier otro método.** Si el cliente se construye sin
negociación de versión, nace asumiendo que el core siempre estará de acuerdo con él.

### 2.1 Lo que falta especificar y nadie ha hecho

- **Config:** el formato de `regulus.toml` y qué va dentro. El principio **Flexible**
  exige que casi todo sea configurable, pero el fichero no está diseñado. Hace falta
  antes de la fase E.
- **Atajos de teclado.** Alexander vive en kitty + tmux: es usuario de teclado. El
  principio **Personalizable** exige que sean reconfigurables. Sin decidir.
- **Qué pasa al reconectar** tras caerse el WebSocket. `seq` en `terminal.data` existe
  para detectar huecos, pero la política de recuperación no está escrita.

---

## 3. El razonamiento, comprimido

Las conclusiones están en los documentos. Aquí está **por qué**, que es lo que se pierde
al leer solo conclusiones.

### 3.1 Por qué módulos in-process y no procesos externos

La pregunta parecía ser "¿aislamiento o velocidad?". No lo era.

IPC y versionado de protocolo existen para cruzar una **frontera de confianza**. Al
principio recomendé procesos externos, con el argumento de que hcom se enchufaría sin
tocarlo. **Ese argumento era falso**: la doctrina Predator
([`01-vision.md` §2](01-vision.md)) dice que el código ajeno se absorbe y se reescribe.
Los módulos son código propio, en Rust. No hay frontera de confianza que cruzar, así que
el coste de IPC se paga sin recibir nada.

**Si alguien vuelve a proponer procesos externos:** la puerta sigue abierta sin coste. Un
`ProcessModule` que implemente el trait `Module` y hable por stdio encaja sin que el core
cambie. Esa es la respuesta, no reabrir la decisión.

### 3.2 Por qué Tauri y no egui/iced, siendo un proyecto Rust

**El argumento decisivo es la terminal, no la preferencia.** xterm.js es el único
emulador de terminal probado en producción que se puede incrustar. Las alternativas en
Rust (`egui_term`, `iced_term`) obligarían a escribir un renderizador VTE, que es un
proyecto entero.

Y la decisión es **barata de revertir**: por los dos límites
([`02-architecture.md` §1](02-architecture.md)), la GUI es un cliente. Cambiarla es
escribir otro cliente, no reescribir Regulus.

### 3.3 Por qué el cliente va antes del core

Porque el core es un servidor de socket, el cliente se construye contra un mock que
devuelve JSON enlatado — y **la lista de métodos del mock es la especificación del core**.
El trabajo de UI produce la spec como subproducto.

La regla que lo hace honesto: **el mock no inventa métodos cómodos para la UI.** Si un
método devuelve un objeto perfectamente formado para un componente concreto, es un método
falso. Los métodos devuelven estado del sistema; la UI compone.

Prueba de que funcionó: al conectar el core real (fase E), **la UI no debe cambiar una
línea.** Si cambia, algún método del mock era falso.

### 3.4 Por qué la matriz de ablación existe

Alexander dijo: *"puedas quitar algo y que todo siga funcionando bien"*. Las intenciones
se degradan en silencio, así que se convirtió en un test: con N módulos, arrancar N veces
quitando uno cada vez.

**Esto es lo que mantiene honesta la doctrina Predator.** Absorber código ajeno tiende a
fundirlo con el core; la matriz es lo que detecta que ha pasado. **Tiene que existir antes
de absorber hcom**, o se pierde la modularidad sin que nadie se dé cuenta.

Efecto secundario que decidió otra cosa: descartó `inventory` y `linkme` para el registro
de módulos. Un módulo que se auto-registra antes de `main` es un módulo que no puedes
quitar con un flag.

### 3.5 Por qué `project_id` está en todo desde el día 1

Alexander eligió multi-proyecto por el principio **Flexible**. La consecuencia fuerte:
**ni el core ni ningún módulo guarda "el proyecto actual"** — es estado del cliente. Así
dos clientes (escritorio y móvil) miran proyectos distintos contra el mismo core.

Es el ejemplo canónico del guardarraíl de [`00-principles.md`](00-principles.md):
`project_id` en el modelo es barato hoy y carísimo mañana → se hace ahora. Sincronizar 20
proyectos no lo ha pedido nadie → no se hace.

### 3.6 Por qué el PTY vive en el core y no en el cliente

Orca lo tiene en Node, dentro del proceso de la app: cerrar la ventana mata al agente.
Aquí `portable-pty` corre en el core, así que un agente sobrevive al cierre de la ventana
y el móvil puede adjuntarse al mismo PTY. Es una mejora real sobre Orca, no una copia.

### 3.7 Por qué ACP importa y qué no resuelve

Existe un protocolo estándar (ACP) que ~45 agentes ya hablan. El número que lo justifica:
`src/hooks/` de hcom son **~549 KB de Rust** de pegamento por agente, el ~10% del
proyecto, que se rompe cada vez que un CLI cambia su TUI.

**Corrección importante** — en un momento escribí que "ACP no cubre agente↔agente". Es
cierto como feature del protocolo pero lleva a la conclusión equivocada: si Regulus tiene
sesión con A y con B, y A invoca una herramienta "mándale esto a B", Regulus hace
`session/prompt` a B y B lo recibe **como mensaje de usuario**. Eso es hcom, construido
sobre ACP. Lo que Alexander valora de hcom (`inject_text` + `inject_enter` en
`delivery.rs:458`) no se pierde.

**Diseño resultante: los dos.** ACP primero; el inyector de PTY absorbido como fallback
para agentes sin ACP y para sesiones que el humano ya tiene abiertas en su terminal.

---

## 4. Descartado — no lo re-propongas

Cada uno tiene una razón concreta. Si vas a reabrir alguno, trae un argumento nuevo.

| Idea | Por qué no |
|---|---|
| **Procesos externos como mecanismo principal** | No hay frontera de confianza (§3.1). La puerta queda abierta vía `ProcessModule`. |
| **WASM / `extism`** | Los casos de uso necesitan PTYs, procesos hijos y sockets. Un sandbox con esos agujeros no es un sandbox. |
| **gpui** (el de Zed) | No es lib estable para terceros. Docs casi inexistentes. API cambia sin aviso. |
| **Electron** | Contradice el requisito de Rust; el core es Rust igual. |
| **React Flow** | La topología es de solo lectura, derivada de manifests. `dagre` + SVG bastan y no sugieren que puedas cablear con el ratón. |
| **`inventory` / `linkme`** | La auto-registración pelea con la matriz de ablación (§3.4). |
| **`node-pty`** | Es de Node. `portable-pty` hace lo mismo y deja el PTY en el core (§3.6). |
| **Un editor de código dentro de Regulus** | Zed lo hace mejor. Decisión explícita de Alexander. |
| **headroom / token-proxy como parte de Regulus** | Son de su cadena personal. Lo aclaró explícitamente: los proxies eran **un ejemplo**, no un plan. |
| **Analizar su tasa de abandono de proyectos** | Se hizo, produjo los 5 requisitos de [`01-vision.md` §5](01-vision.md), y Alexander cerró el tema. **No lo reabras.** |

---

## 5. Cómo trabajar con Alexander

| Cosa | Cómo |
|---|---|
| **Idioma** | Español, directo, sin rodeos. Código, UI y commits en inglés (decisión #20); los docs en español, solo eso. |
| **Formato de salida** | **El modo `i-have-adhd` está activo** (`~/.claude-sub/skills/i-have-adhd/`): primera línea = la acción, listas numeradas, máximo 5 ítems por grupo, cero preámbulo, cero despedida, estimaciones en unidades concretas, y terminar con **una** acción siguiente. |
| **Decisiones** | Preséntale 2-4 opciones con el coste real de cada una y tu recomendación primero. Responde en prosa, no con una palabra, y a menudo cambia la premisa de la pregunta. Lee bien lo que contesta. |
| **Cuando corta un tema** | Lo corta. "No me importa" significa que se acabó. No lo retomes de lado. |
| **Cuando te corrige** | Corrige el documento afectado, marca visiblemente la corrección, y sigue. No te disculpes ni lo rumies. |
| **Qué valora** | Que le digas cuando su instinto es correcto **y por qué el motivo es mejor que el que él daba**. Y que le señales lo que está mal. No quiere un sí-señor. |

---

## 6. Estado del repositorio

```
AGENTS.md                  ← punto de entrada. CLAUDE.md es symlink a este
README.md                  ← cara humana, branding
docs/00-principles.md      ← manda sobre todos los demás
docs/01-vision.md … 08-design-system.md
docs/research.md           ← datos medidos
docs/HANDOFF.md            ← este fichero
docs/ideas/boxes.md        ← sin definir, bloquea Isolation
assets/branding/           ← logo, icono, paleta, build_assets.py
```

- **Git: `master`, cero commits.** Nada está confirmado. Tarea nº1.
- **No existe `Cargo.toml`, ni `crates/`, ni `ui/`.** Cero código de aplicación.
- **Hay otros agentes trabajando en paralelo.** El branding
  (`assets/branding/`) y `08-design-system.md` los produjeron otros agentes durante esta
  sesión, no yo. Si un fichero cambia bajo tus pies, es probable que sea eso: toma el
  estado del disco como verdad y no lo revientes.

---

## 7. Las dos decisiones abiertas

| # | Qué | Quién la resuelve | Qué bloquea |
|---|---|---|---|
| **#10** | Mecanismo de registro de módulos | BRIEF-3. Recomendación fuerte: **a mano** (§3.4) | Fase E |
| **#11** | El sistema de cajas | **Solo Alexander.** BRIEF-5 es una entrevista, no un diseño. Lee antes el RFD de ACP Proxies | `Isolation` |

Para la #11, lo único que ya se puede afirmar: **quitar una caja debe dejar el módulo
funcionando.** Si un módulo solo funciona dentro de su caja, la caja es parte del módulo y
no debería ser una caja.

---

## 8. Las tres trampas de este proyecto

1. **Construir el framework y nunca las features.** Mitigación activa: toda capacidad del
   core necesita un caso de uso de [`01-vision.md` §4](01-vision.md) que la pida.
2. **Absorber hcom antes de que exista la matriz de ablación.** Es cómo se pierde la
   modularidad sin enterarse. El orden del TODO lo impide: hcom va después de v1.
3. **Una UI bonita y un core que nunca llega.** Riesgo propio de empezar por el cliente.
   Mitigación: el mock **es** la spec, y la fase D extrae `regulus-proto` de él, así que
   la fase E no empieza de cero.
