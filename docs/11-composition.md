# 11 — Composición: el modelo de Regulus

> **v0.3** · 2026-10-05 · ✅ **DECIDIDO por Alexander** (2026-10-05) — sale de la
> conversación del 2026-10-04. Respuestas a lo abierto en §12. Pendiente: aplicar la lista
> de §11 a los demás docs. La versión en llano está en [`concept.md`](concept.md).

---

## 1. El modelo en un dibujo

```
Perfil  (global por defecto, o uno propio de un proyecto)
 ├─ Workflow principal          ← canvas principal
 ├─ Workflows extra             ← cada uno en su canvas
 └─ Workflow de cada módulo     ← cómo funciona el módulo por dentro
       └─ Nodos                 ← piezas que trae cada módulo

Vista unificada: todos los workflows del perfil en un solo canvas.
Ejecución: una copia en marcha de un workflow. Puede haber varias a la vez.
```

> *"Como un lego pero rápido."* · *"Sin AI no hay nada, pero el core como tal no es
> nada: existen módulos, y hay cosas imprescindibles."* — Alexander

---

## 2. La base (core)

La placa del lego. **No tiene funciones propias**: no sabe qué es un agente, un modelo
ni una skill. Solo:

1. **Registro** de módulos y de los nodos que traen.
2. **Cables** de los dos tipos de §4.
3. **Bus** de eventos (los cables "luego" corren sobre él).
4. **Store** con namespace por módulo.
5. **Proyecto** (`project_id` en todo, decisión #15).
6. **Ejecuciones**: arrancar, parar y listar copias en marcha de un workflow.

**Sale del core:** `Agent`, `Isolation` y procesos/PTY. Pasan a módulos.

---

## 3. Módulos y nodos

**Un módulo es un paquete instalable que trae nodos** y un workflow propio (§6). Como
los paquetes de nodos de ComfyUI.

| Módulo | Nodos que podría traer |
|---|---|
| **Comunicación** (de hcom) | Enviar, Bandeja, Entregar, Participante |
| **Agente** | Agente |
| **Proveedores** | OpenAI, Local, … (ofrecen `llm.chat`, `llm.vision`…) |
| **Modelos** | Rotar modelo, Elegir modelo |
| **Procesos** | Proceso, Terminal |

Además de nodos de módulo hay **recursos**: skills, documentos de research, prompts. Son
ficheros, se conectan como un nodo más y añadirlos **no requiere recompilar**
(principio Personalizable).

**Comunicación habla con *participantes*, no con agentes:** un agente, Alexander en la
terminal o un script. Así no depende del módulo de agentes.

**Comunicación, base construida (2026-10-18).** Módulo `communication`, con
`[module.origin]` en honor a aannoo (hcom). Un nodo **Canal** recibe por su entrada
`participants` (varias) a quien ofrezca `participant@1`; la caja Agente lo ofrece y cada
mensaje le llega como un prompt en su sesión ACP, lo mismo que hcom conseguía tecleando
en la terminal (`old/07-ecosystem.md` §1.4). Un mensaje con `@nombre` va a esos
participantes; sin menciones, a todos menos al remitente. Cada participante tiene su cola
y recibe en orden. Lo que contesta queda **registrado, no entregado**, para que dos
agentes no se respondan sin fin. El historial vive en el directorio del nodo y sobrevive
a las ejecuciones. CLI: `regulus msg send` y `regulus msg log`. Probado con opencode y
Claude en el mismo canal, y con `--without regulus/communication` las cajas siguen
funcionando. Falta que un agente pueda **enviar** por su cuenta: una herramienta MCP en
su sesión. El inyector de PTY de hcom, para agentes sin ACP, sigue aplazado.

### 3.0 Comunicación como caja líquida

✅ **DECIDIDO (Alexander, 2026-10-18).** La base de arriba era un método fijo, no una
caja. Comunicación es un módulo propio (no lanza hcom) y **ACP es su corazón para
entregar y observar**, pero solo como una vía más.

**Qué hace y cómo llega van separados.** Cada capa es su propio módulo:

```
 Comunicación          Herramientas (módulo aparte)       Agente
 send · list · inbox → motor: MCP │ propio │ CLI      → ranura tools
 (operaciones)          formato: JSON │ TOON │ …
```

- **Comunicación** ofrece operaciones con nombre y tipos por una capacidad neutra
  (`tool@1`, nombre provisional). No sabe qué es MCP.
- **Herramientas** recoge operaciones de cualquier módulo y se las presenta al agente.
  Motor intercambiable (MCP hoy; protocolo propio o vía CLI mañana) y **formato como
  envoltura** (JSON hoy; TOON u otro mañana). Otras cosas que hablen protocolos de fuera
  seguirán la misma regla: el protocolo es un módulo aparte de lo que transporta.
- **Agente** tiene una ranura `tools` y monta lo que le conecten (ACP pasa los servidores
  al abrir la sesión).

Límite conocido: Claude Code y opencode solo aceptan herramientas por MCP, que exige
JSON-RPC por fuera; con ellos, otro formato solo cabe dentro de los resultados.

**Orden:** 1 · reformar en caja: identidad por agente, estado vía ACP, cola que espera a
que el agente quede libre, ACP como vía · 2 · Herramientas con motor MCP y formato JSON
(send/list/inbox) · 3 · observar y choques: ediciones vistas por ACP, aviso < 30 s ·
4 · suscripciones, hilos e intención · 5 · PTY y relay. TOON y el protocolo propio
quedan como puertas abiertas, sin construir.

**Paso 1, hecho (2026-10-18).** `participant@1` gana `participant.describe`: nombre
(`@nombre`, único en el canal), tipo y estado (`idle`, `busy`, `gone`, `unknown`). La caja
Agente toma el nombre de su campo `name` (vacío: el id del nodo) y el estado de su motor
por `engine.session.status`, que ACP conoce. El canal espera a que el participante quede
libre antes de entregar: pregunta cada `poll_ms` (500) y se rinde a los
`max_wait_seconds` (600); un participante que no dice su estado recibe sin esperar.
CLI: `regulus msg who` muestra nombre, estado y mensajes en cola. Probado con opencode y
Claude: dos mensajes a un agente ocupado esperan su turno y se entregan en orden.

**Paso 2, hecho (2026-10-18).** Tres módulos, cada uno cambiable por separado:

| Pieza | Módulo | Qué sabe |
|---|---|---|
| Operaciones | `communication` (salida `tools`, `tool@1`) | `send`, `participants`, `inbox`; nada de MCP |
| Caja Herramientas | `tools` (`tools` → `toolset`) | junta operaciones, nombre de quien llama, formato |
| Motor | `tools-mcp` (`tools.engine@1`) | MCP por stdio; nada de mensajes |

- `tool@1` (`tool.list`, `tool.call`): operaciones con esquema JSON; el resultado es
  dato, no texto. `toolset@1` (`toolset.mount`) y `tools.engine@1`
  (`tools.engine.mount`) comparten la forma `Mount` (protocolo, orden, argumentos,
  entorno).
- La caja Agente monta cada `toolset` **en nombre de su campo `name`**: el agente no
  puede enviar como otro. Cada montaje recibe un vínculo aleatorio que viaja en el
  entorno del puente, no en sus argumentos.
- El formato es el campo `format` de la caja (`json`); TOON será otra opción ahí. El
  protocolo es el motor: otro protocolo es otro módulo conectado a `engine`.
- El motor ACP pasa los montajes `mcp-stdio` en `session/new`; uno que no hable el
  protocolo se queda fuera con aviso. El puente es el mismo binario (`regulus
  tools-mcp`, oculto) apuntando al mismo `--home`.
- Los permisos de la caja Agente valen también para las herramientas: con `reject`,
  Claude no puede llamarlas.

Probado: Claude envió por su cuenta a opencode, se entregó por ACP y la respuesta quedó
registrada; opencode leyó su bandeja con `inbox`.

✅ **El ciclo** (Alexander, 2026-10-18). Agente → Canal (`participants`) → Herramientas →
Agente es un ciclo, y el agente necesita la caja y el canal activos para montar y listar.
La entrada `participants` del canal es `lazy`: el canal solo la lee en marcha, así que
`start_order` ignora sus cables y arranca canal → caja → agentes, se declaren como se
declaren. El core solo mira el flag del puerto, no el módulo.

✅ **El bucle** (Alexander, 2026-10-18). Dos agentes pueden contestarse sin fin por
`send`. El canal limita los envíos de cada agente por la herramienta: campos
`send_limit` (10) y `send_window_seconds` (60); 0 es sin límite. Pasado el límite, `send`
devuelve un error que el agente lee. Lo que envía una persona por la CLI no cuenta.
Probado: un agente que reintentaba sin parar quedó en `send_limit` envíos por ventana.

### 3.1 Conjunto base

La IA es el centro del *propósito*, no de la *estructura*. Regulus viene de serie con los
módulos imprescindibles para usarlo: comunicación, agente, proveedores, modelos y
procesos.

- Imprescindibles para **usarlo**, no para que arranque: la matriz de ablación los sigue
  quitando uno a uno.
- La lista vive en **configuración** (`[base] modules = [...]`), no en el core. El core
  sigue sin conocer ningún módulo por nombre.

---

## 4. Cables: dos tipos

| Cable | Dibujo | Significa | Ejemplo |
|---|---|---|---|
| **Usa** | discontinuo `⇢` | Este nodo usa a ese como servicio, de forma permanente | Agente ⇢ Proveedor |
| **Luego** | continuo `→` | Cuando pasa esto, haz aquello | Mensaje recibido → Entregar |

- **Usa** une una salida con una entrada del mismo tipo (`llm.vision` no entra en
  `llm.chat`). Las entradas declaran `required` (`03-modules.md` §1.1).
- **Luego** lleva un **dato tipado por capacidad** (`text@1`, `image@1`…) y lo entrega el
  bus, a cada entrada "luego" cableada con la misma capacidad y versión
  (`02-architecture.md` §6). No hay motor de flujo aparte: un Bucle o un Switch son nodos
  de `flow` que reciben y emiten por "luego" (`13-blocks.md` §2). `lazy` y `cardinality`
  valen tal cual.
- **Riesgo:** bucles (A dispara B, B dispara A). Red de seguridad del core: corta la cadena
  al pasar de `[bus] max_depth` entregas (por defecto 64) y lo reporta en el bus. El tope N
  de un Bucle es del nodo, no del core.
- **Autoconexión:** si para una entrada "usa" hay exactamente una salida compatible, se
  conecta sola. Recién instalado, todo funciona sin cablear.

---

## 5. Workflows

**Un workflow es una cadena de funcionamiento:** qué nodos hay y cómo están cableados.

- **Principal:** el canvas por defecto del perfil.
- **Extra:** canvas aparte, para separar cosas. Se comunican con los demás por eventos (un
  cable "luego" que cruza de canvas) o referenciando un nodo de otro workflow.
- **Vista unificada:** todos los workflows del perfil en un solo canvas, incluidos los de
  los módulos.

Se ven y editan de tres formas que son lo mismo: el canvas, el fichero
(`regulus.toml`) y el CLI (`regulus connect a.out b.in`, `regulus graph`). Principio
Infraestructura.

**Puertos propios y hablarle** ✅ (2026-11-07). Un workflow declara entradas y salidas
(`ports`, mismos `Port` que un nodo); dentro se cablean como `self.<puerto>`. Así se
enchufa en otro workflow del perfil como una caja (§6) y funciona igual solo, sin nadie
fuera. A uno que corre se le habla con `workflow.send` / `regulus send <ejecución>
<puerto> <valor>`, y la respuesta sale en `regulus events` emitida desde `self.<salida>`.
Ejemplo: `crates/modules/hello/examples/greeter.toml`.

---

## 6. El workflow de cada módulo

Cada módulo trae **su workflow interno ya armado, que funciona sin tocar nada**. Por
ejemplo, Comunicación: recibir → encolar → entregar → reintentar.

- **Se ve todo** lo que ejecuta. Sin cajas negras.
- **Se edita solo lo que el módulo expone.** Quien escribe el módulo decide qué es
  ajustable; el resto se ve pero no se toca. Como los grupos de nodos de Blender.
- Se pueden **importar nodos** de otros módulos dentro de ese workflow, por los mismos
  cables.

Lo mismo vale para un workflow del usuario: un nodo **sin módulo** cuyo
`internal_workflow_id` apunta a un workflow con puertos es una **caja**; el core la
ejecuta en línea (sus nodos corren como `caja/nodo`) y el bus cruza sus puertos en ambos
sentidos, a cualquier profundidad (`02-architecture.md` §4 regla 4, §6). Un nodo con
módulo y workflow interno sigue siendo del módulo.

---

## 7. Perfiles

**Un perfil es el conjunto de workflows y ajustes con el que trabaja un proyecto.**

- Hay un **perfil global por defecto** (`~/.regulus/`).
- Un proyecto puede **heredar** del global y cambiar solo lo suyo, o tener uno **desde
  cero**.
- Capas de ajustes, cada una pisa a la anterior:
  `defaults del módulo → global → plantilla → proyecto`.

**Plantilla:** un workflow o perfil guardado con huecos (`{{model}}`) y con la lista de
módulos que necesita (`requires`, con versión). Aplicarla copia; el proyecto es dueño de
su copia.

**Plantilla, construida** ✅ (Alexander, 2026-11-08: opción 1A, la aplica el core, no un
módulo). Es un fichero de perfil TOML con una tabla más, `[template]`, que se quita antes
de leerlo como perfil:

```toml
[template]
name = "greeting"                     # lo que se escribe en `apply <nombre>`
description = "..."
requires = [{ module = "regulus/files", version = ">=0.1" }]   # id y requisito semver
[template.vars]
who  = { description = "..." }                         # sin default: obligatoria
file = { description = "...", default = "greeting.txt" }
```

- **Huecos:** `{{nombre}}` en cualquier sitio del resto del texto. Sustitución **literal**
  en una pasada: el valor entra tal cual y no se vuelve a leer; por eso un valor (o el
  default que se use) con `"`, `\` o caracteres de control se rechaza.
  Sin escapes ni expresiones. `{{…}}` que no es un nombre (`{{.}}`, `{{json.q}}`) se deja.
  Un hueco sin declarar, una var sin valor ni default o un `--set` que no existe son error.
  Para dejar el `{{input}}` de un módulo: `input = { default = "{{input}}" }`.
- **Dónde:** `[templates] paths` (por defecto `<home>/templates`), leídas solo al pedirlas;
  las del repo, en `templates/`. `apply` acepta también una ruta.
- **Aplicar** comprueba `requires` contra el registro (id y versión del manifiesto) y
  `profile.check` (un error no escribe; los avisos vuelven como `diagnostics`), y escribe
  `<id>.toml` en la primera ruta de `[profiles] paths`, cargado sin reiniciar. Nunca pisa
  sin `--force`; el id es un nombre de fichero simple.
- `regulus template ls` · `show <nombre>` · `apply <nombre|ruta> --set k=v [--id x]
  [--force]`; por RPC, `template.list`, `template.get` y `template.apply`.

**Guardar desde la app** ✅ (2026-11-08): la app guarda el perfil entero (`profile.save`,
mismas reglas de escritura que `apply`) y lo borra (`profile.delete`, no mientras corre). El
fichero se reescribe desde el perfil: **los comentarios se pierden**, la copia es de la app.
Lo que ya corre sigue con el workflow con que arrancó; una ejecución hija que arranque
después lee el perfil guardado. CLI: `regulus profile save <fichero|-> [--force]` ·
`profile rm <id>` · `module show <id>` (el manifiesto, `module.manifest`).

---

## 8. Ejecuciones: el plano y lo que corre

| Plano (lo que se diseña) | En marcha (lo que pasa) |
|---|---|
| Perfil, workflow, nodo | **Ejecución:** una copia viva de un workflow |
| Vista canvas (TOPOLOGÍA) | Vista OPERAR (`04-client.md` §2) |

- Un mismo workflow puede tener **varias ejecuciones en paralelo** (tres agentes dev con
  el mismo workflow).
- **Varios proyectos en paralelo**, cada uno con su perfil. El core los sirve todos a la
  vez (decisión #15).
- El canvas puede **marcar** qué nodos están vivos, pero no mezcla estado con diseño.

---

## 9. Aislamiento y secretos

**Dos niveles de aislamiento por proyecto:**

1. **De configuración:** cada proyecto, su perfil. Ya cubierto por §7.
2. **De ejecución:** los nodos del proyecto A no ven ficheros, claves ni memoria del B.
   Es UC-4 y es lo que de verdad protege.

**Secretos fuera de los workflows.** Un workflow guarda solo referencias
(`secret:openai`); las claves viven en `~/.regulus/secrets` (o el llavero del sistema).
Así compartir una plantilla o un módulo nunca comparte una clave.

✅ **Cómo se resuelven** (Alexander, 2026-10-17: *"lo más flexible"*):

- **Los resuelve el core**, no los módulos. Al entregar a un módulo la configuración de
  un nodo, cada valor `secret:<nombre>` se cambia por la clave. Ningún módulo conoce el
  almacén, así que cambiar de almacén no toca ningún módulo.
- **Los almacenes son una lista ordenada** en `config.toml` (`[secrets] sources`). Hoy
  solo existe `file` (`<casa>/secrets.toml`, permisos 0600). El llavero del sistema será
  otra fuente cuando alguien la pida; la puerta queda abierta.
- **Un secreto que no existe** hace fallar la llamada con un mensaje que dice cómo
  crearlo. Nunca llega vacío al módulo.
- **CLI y GUI por igual:** los métodos `secret.set`, `secret.list` (solo nombres) y
  `secret.delete`, y el comando `regulus secret`. Los valores no aparecen en los logs ni
  en las respuestas.

### 9.1 Las cajas (decisión #11) se resuelven aquí

Las tres lecturas de [`old/ideas/boxes.md`](old/ideas/boxes.md) caben en este modelo **sin
mecanismo propio**:

| Lectura | En este modelo |
|---|---|
| A — envoltorio / proxy | Un nodo metido en un cable "usa": Agente ⇢ **Proxy** ⇢ Proveedor |
| B — contenedor aislado | Un **grupo** de nodos en el canvas con límites (worktree, MCP, entorno, memoria) |
| C — preset de flags | Un perfil o una plantilla |

Si se aprueba, `Isolation` deja de ser un struct del core y pasa a ser los límites de un
grupo. La #11 se cierra.

La lectura A, generalizada, es la regla 4 de la caja líquida (`00-principles.md`): toda
caja que entra y sale por la misma capacidad es una **envoltura** y se mete en cualquier
cable. Los proveedores son el primer caso (`12-agents.md` §6.4).

---

## 10. Instalar módulos y compartir

El marketplace es de **módulos hechos para Regulus**, plantillas y recursos. Compatible
con la doctrina Predator: el código ajeno se reescribe como módulo de Regulus, y eso es lo
que se instala.

**Instalar** (los módulos son crates in-process, decisión #2):

| Opción | Qué es instalar | Coste |
|---|---|---|
| **A. Recompilar automático** (recomendada v1) | `regulus module install X` añade el crate y recompila | Segundos-minutos. Cero diseño nuevo |
| B. Librerías dinámicas | Cargar un `.so` en caliente | ABI de Rust inestable; complica la ablación |
| C. `ProcessModule` | Proceso aparte por stdio | IPC; pero aísla módulos de terceros sin revisar |

**Barato, se hace ya:** ids `autor/módulo@versión`; plantillas con `requires` versionado;
ficheros autocontenidos. **No se hace todavía:** índice remoto, descarga, firma.

---

## 11. Qué cambió en otros documentos

✅ Aplicado el 2026-10-05: `02-architecture.md` §4 (el core solo conoce piezas
genéricas), `03-modules.md` (nodos, puertos con versión, campos, workflow interno) y
`04-client.md` (Canvas + Operar). Los documentos del modelo anterior están en
[`old/`](old/README.md); lo que seguía sirviendo, en [`plan.md`](plan.md) §4.

---

## 12. Respuestas de Alexander (2026-10-05)

> *"Lo que tenemos que tener primero es el funcionamiento básico que permita hacer
> cualquier cosa de estas en el futuro."* — Alexander

La base deja sitio para todo y decide lo mínimo (guardarraíl de `00-principles.md`).

- ✅ **Cardinalidad:** no se decide. Cada entrada declara `uno | varios`; por defecto
  `uno`. Rotar proveedores es un nodo (*Rotar modelo*), no varios cables.
- ✅ **Contrato de cada tipo de puerto:** no se decide. Cada tipo lleva versión
  (`llm.chat@1`); el formato lo fija el primer módulo que lo use.
- ✅ **UI de un módulo:** por defecto, el sistema de Regulus — el módulo declara campos
  y la UI los dibuja sola. Un módulo puede traer su propia UI si la necesita.
- ✅ **Workflow interno de un módulo:** se edita en la interfaz; Regulus lo genera como
  Rust y lo compila.
  - 🔶 **Cómo encaja con Personalizable** (*"se cambia sin recompilar"*): recompila
    **quien crea el módulo**, una vez al crearlo o publicarlo. **Quien lo usa** solo toca
    lo que el módulo expone, que se guarda en fichero y se aplica sin recompilar.
- ❓ **Aislamiento de ejecución** (§9, nivel 2): fuera de v1. Se decide más adelante.

---

## 13. Proyectos y `regulus.toml` (2026-11-08)

✅ **DECIDIDO** (2026-11-08, Alexander: "lo más flexible posible, nada rígido") — un proyecto
tiene su `regulus.toml`, puede traer módulos solo suyos y el fichero tolera lo que no entiende.
Lo que había, la forma y lo que cuesta.

**Lo que ya existe en el código.** Un proyecto es solo un `project_id: Option<String>` en
perfiles, ejecuciones y eventos (`regulus-proto/src/graph.rs`); `profile.list`,
`execution.list` y `event.list` filtran por él, el store le da `store/projects/<id>.db` y los
nodos `data/projects/<id>/…`. No hay `struct Project`, ni carpeta de proyecto, ni fichero:
`02-architecture.md` §4 describe un `Project { root, vcs }` que no se construyó. Y toda la
configuración (`config.toml`) rechaza claves desconocidas (`deny_unknown_fields`): un error
tipográfico tumba el core y la app.

**La forma.** Un proyecto es **una carpeta con `regulus.toml`**. Todo es opcional; el id es el
nombre de la carpeta si no se dice; las rutas son relativas al fichero; **las claves que el
core no entiende se conservan y se avisan, nunca tumban**. Tres capas, la más cercana gana:
`config.toml` global → `regulus.toml` del proyecto → el perfil.

```toml
[project]
id = "mi-proyecto"              # por defecto, el nombre de la carpeta
name = "Mi proyecto"
profiles = [".regulus/profiles"]    # listas: igual que [profiles].paths del global
templates = [".regulus/templates"]  # cajas y plantillas solo de este proyecto

[modules]
disable = ["web"]               # los nodos de `web` no corren en perfiles de este proyecto
# enable = [...]                # si está, solo estos

[[modules.local]]               # módulo solo de este proyecto, fuera de proceso
manifest = "tools/mi-modulo/manifest.toml"
command = ["tools/mi-modulo/run"]

[client]                        # pisa el [client] global solo aquí (§14 de 04-client, cuando exista)
start_view = "operate"
```

**Qué hace el core con cada parte, y cuánto cuesta:**

| Parte | Qué | Coste |
|---|---|---|
| Registro | `project.list`, `project.get`, `project.open { path }`, `project.close` en el core; la lista de carpetas abiertas vive en `config.toml` `[projects] paths = []` (el cliente la pide, el core la sirve: decisión #15). CLI `regulus project ls\|open\|close\|show`. | ~1 día |
| Perfiles y plantillas del proyecto | Al abrir, el core suma sus carpetas a las fuentes de `profile.*` y `template.*` con `project_id` fijado. Una **caja guardada como plantilla del proyecto es ya un "módulo especial solo para este"**: sin compilar nada ni cambiar el core. | incluido |
| `[modules] disable/enable` | El core carga los módulos una vez (lista estática, decisión #10). Por proyecto no se descargan: se **vetan**: `profile.check` marca los nodos de un módulo vetado como *desactivado* con la razón "disabled in this project" y `execution.start` los rechaza con ese diagnóstico. Honesto y sin tocar el registro. | ~medio día |
| `[[modules.local]]` | **`ProcessModule`** (§10 opción C, la puerta abierta de `02-architecture.md` §2): el core lanza `command`, habla JSON-RPC por stdio con el mismo contrato de `Module` (manifest, `call`, estados), lo lista como `<project>/<id>` y lo quita al cerrar el proyecto. Cualquier lenguaje. La ablación lo cubre: quitarlo deja el resto igual. | ~2-3 días, se hace después del resto |
| Tolerancia | `regulus.toml` se lee con `#[serde(flatten)] extra: toml::Table` y aviso por clave desconocida. **Se propone lo mismo para `config.toml`**: avisar en vez de fallar. | ~1 h |

**Lo que no se decide aquí:** el aislamiento de ejecución (§9 nivel 2, fuera de v1); los
`.toml` de perfil siguen siendo los de §7, no cambian.

**Respuestas de Alexander (2026-11-08):** id = el nombre de la carpeta salvo `[project] id`;
`config.toml` con clave desconocida **avisa y sigue**; orden: registro + perfiles/plantillas
+ veto primero, `ProcessModule` después.

**Construido (2026-11-08)** ✅ — registro, perfiles y plantillas del proyecto, veto y
tolerancia; `ProcessModule` no.

- **Registro:** `project.list` · `get` · `open` · `close` (protocolo 1.12, códigos `-32010` a
  `-32013`, [`composition-preview.md`](composition-preview.md)) y `regulus project
  ls|show|open|close`. `open` crea un `regulus.toml` mínimo (`[project] name`) si no hay, y
  edita solo `[projects] paths` de `config.toml` leyéndolo como tabla: lo demás se conserva,
  **los comentarios no**. `close` no borra nada. Sin `[project] profiles`/`templates`, valen
  `.regulus/profiles` y `.regulus/templates`. El id se valida como el de un perfil; dos
  proyectos con el mismo id no se abren a la vez.
- **Tolerancia:** clave desconocida en cualquier nivel → se conserva (`project.get` la da en
  `content`) y avisa; un `regulus.toml` roto deja los valores por defecto y avisa;
  `[[modules.local]]` se lee, se devuelve y avisa "local modules are not supported yet";
  `[client]` se devuelve tal cual (nadie lo consume aún). `config.toml` igual: avisa al
  arrancar (`regulus: … unknown key`) y sigue; `hello` no devuelve avisos, no tenía dónde.
- **Perfiles y plantillas:** los del proyecto se cargan al abrir con su `project_id` (si el
  fichero dice otro, aviso y gana el proyecto) y se descargan al cerrar; las ejecuciones en
  marcha siguen con su copia. `profile.save` y `template.apply --project` escriben en la
  primera carpeta de perfiles del proyecto. `template.list` los etiqueta con `project_id`.
- **Veto:** por id de manifiesto (`autor/módulo`, cualquier versión); `profile.check` da
  `node.module_disabled` como **aviso** (el perfil sigue siendo válido y se puede guardar) y
  `execution.start` y las llamadas hijas rechazan (`-32013`).

**Desvíos:** (1) el ejemplo `disable = ["web"]` no vale: se nombra por id de manifiesto
(`regulus/web`) y un nombre que no casa con ningún módulo cargado avisa. (2) Una plantilla de
un proyecto aplicada sin `project_id` cae en su proyecto, no en el global. (3) El
`regulus.toml` se relee si cambió (se mira su fecha en cada llamada): el veto y los avisos
siguen al fichero sin reabrir; cambiar el id o las carpetas de perfiles pide cerrar y abrir.
(4) Una ejecución que se reanuda tras reiniciar el core no pasa el veto: corre la copia que
tenía. **Queda:** `ProcessModule` para `[[modules.local]]` y un consumidor de `[client]`
(`04-client.md`).
