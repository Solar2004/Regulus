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
- **Luego** es una suscripción al bus: no hay motor de flujo nuevo. **Riesgo:** bucles
  (A dispara B, B dispara A). El core corta una cadena al pasar de una profundidad
  configurable y lo reporta.
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

---

## 6. El workflow de cada módulo

Cada módulo trae **su workflow interno ya armado, que funciona sin tocar nada**. Por
ejemplo, Comunicación: recibir → encolar → entregar → reintentar.

- **Se ve todo** lo que ejecuta. Sin cajas negras.
- **Se edita solo lo que el módulo expone.** Quien escribe el módulo decide qué es
  ajustable; el resto se ve pero no se toca. Como los grupos de nodos de Blender.
- Se pueden **importar nodos** de otros módulos dentro de ese workflow, por los mismos
  cables.

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

---

## 8. Ejecuciones: el plano y lo que corre

| Plano (lo que se diseña) | En marcha (lo que pasa) |
|---|---|
| Perfil, workflow, nodo | **Ejecución:** una copia viva de un workflow |
| Vista canvas (TOPOLOGÍA) | Vista OPERAR (`04-client.md` §2.1) |

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

### 9.1 Las cajas (decisión #11) se resuelven aquí

Las tres lecturas de [`ideas/boxes.md`](ideas/boxes.md) caben en este modelo **sin
mecanismo propio**:

| Lectura | En este modelo |
|---|---|
| A — envoltorio / proxy | Un nodo metido en un cable "usa": Agente ⇢ **Proxy** ⇢ Proveedor |
| B — contenedor aislado | Un **grupo** de nodos en el canvas con límites (worktree, MCP, entorno, memoria) |
| C — preset de flags | Un perfil o una plantilla |

Si se aprueba, `Isolation` deja de ser un struct del core y pasa a ser los límites de un
grupo. La #11 se cierra.

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

## 11. Qué cambia en otros documentos (al aprobar)

| Doc | Cambio |
|---|---|
| `01-vision.md` §4 | UC-1 se parte en Comunicación y Agente. Nuevos UC: Proveedores, Modelos, Recursos |
| `01-vision.md` §4.1 | "Identidad de agente" sale de las capacidades del core |
| `01-vision.md` §3 | "No es un marketplace de plugins" → sí de módulos hechos para Regulus |
| `02-architecture.md` §4 | ✅ Aplicado (v0.4). `Agent` e `Isolation` salen del core; entran cables, perfiles y ejecuciones |
| `03-modules.md` | ✅ Aplicado (v0.4). Un módulo trae nodos y un workflow interno con parámetros expuestos; ids con autor y versión |
| `04-client.md` | TOPOLOGÍA = canvas editable; OPERAR = ejecuciones. Los 8 métodos se rehacen (ver `05-roadmap.md` §1.0) |
| `HANDOFF.md` §4 | React Flow se reabre (cablear con el ratón es ahora el requisito). Un módulo proxy de Regulus no es la cadena personal headroom/token-proxy |
| `ideas/boxes.md` | Se cierra con §9.1 |

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
