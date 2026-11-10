# 14 — Code: una carpeta para que un agente programe

> **v1.0** · 2026-11-10 · ✅ *Construido* según el contrato de Regulus Code (Alexander,
> 2026-11-09). 🔶 Lo que el contrato no fijaba y se decidió al construir va marcado y
> reunido en §10. Lee antes [`12-agents.md`](12-agents.md) §8 (el motor nativo) y
> [`13-blocks.md`](13-blocks.md) §3.3 (`delegation`, `human`).
>
> **Nada de este documento se ha corrido contra un modelo real:** todos los modelos de
> las pruebas son servidores falsos.

---

## 1. La idea

**Regulus Code no es un agente nuevo: es una plantilla.** El bucle es el motor nativo, la
conversación es la de la caja Agente, quien decide un permiso es el Decisor de `human` y
los trabajadores son un Delegado. Lo único que faltaba era **la carpeta**, y eso es lo
único que añade el módulo `regulus/code`:

```
 [Agente] ⇢ engine ⇢ [native] ⇢ model   ⇢ Proveedor
     ⇣ decisions                ⇢ tools   ⇢ [workspace]  code_*  (ficheros, git, shell)
 [Decisor]                      ⇢ tools   ⇢ [workers]    worker_*
     ⇡ decisions                ⇢ context ⇢ [instructions] ⇢ workspace
 [workers] ⇢ engine ⇢ [worker_engine] ⇢ tools ⇢ [reader] ⇢ workspace   (solo lectura)
```

El módulo **no llama a ningún modelo y no guarda ninguna conversación**. Es dueño de lo
que pasa dentro de la carpeta:

| Garantía | Cómo |
|---|---|
| Ninguna operación de ficheros sale de la raíz | cada ruta se resuelve (enlaces y `..`) **antes** de compararla con la raíz |
| Nada entra en `.git` | nombres protegidos (`protect`): ni listar, ni leer, ni buscar, ni escribir |
| Toda respuesta tiene tope | bytes, entradas, coincidencias, tiempo: campos del nodo |
| Una edición solo cae donde el texto coincide | `old_text` exacto y una sola vez; si no, conflicto y no cambia nada |
| Un comando acaba y no deja nada | grupo de procesos propio, plazo, SIGTERM → SIGKILL, y lo que dejó en segundo plano se va con él |
| git no ejecuta nada del repo | sin fsmonitor, hooks, diff externo, textconv, filtros ni pager; no reescribe el índice |

**La shell no es un sandbox.** `code_run_command` arranca en la raíz, pero un comando
puede leer y escribir lo que pueda el usuario que corre Regulus. Aprobar un comando es
fiarse de ese comando. Quién puede llamarlo lo decide quien presenta las herramientas
(la política del motor, un hook, una persona), no este módulo.

**Las seis preguntas** (`00-principles.md`): (1) ningún número en la lógica: los 29
límites y programas son campos con valor por defecto; (2) la matriz de ablación pasa con
el módulo y sin él (§9); (3) medido, §8; (4) todo se cambia en el perfil, sin recompilar;
(5) `code.inspect` se llama con `regulus call` y el cliente de terminal añade `regulus
code` (`04-client.md`); (6) el workspace se cambia por cualquier nodo que ofrezca
`code.workspace@1` y la vista y las instrucciones siguen funcionando (probado con un
módulo falso).

---

## 2. Los nodos

Módulo `regulus/code@0.1.0`, crate `crates/modules/code`. Tres tipos de nodo.

### 2.1 `workspace` — la carpeta

| Puerto | | Qué |
|---|---|---|
| `workspace` | salida "usa", `code.workspace@1` | la carpeta para otras cajas (§4). **Los clientes encuentran un workspace por esta capacidad, nunca por el id del nodo** |
| `tools` | salida "usa", `tool@1` | sus operaciones como herramientas (§3) |
| `root` | entrada "luego", `path@1` | mueve la carpeta: un texto, o un objeto con `path` (el `info` de un `workspace` del módulo `workspace`: su `ready` aquí da un worktree por ejecución). Texto vacío: vuelve al campo. Un valor que no es una carpeta absoluta existente se rechaza y **se guarda**: lo siguiente falla en vez de seguir en la carpeta anterior. Se guarda en checkpoint |
| `changed` | salida "luego", `json@1` | `{tool, path, bytes, created, by}` tras cada escritura o edición |
| `ran` | salida "luego", `json@1` | `{command, cwd, code, signal, timed_out, ms, by}` tras cada comando, **sin su salida**: lo que imprime un programa no va al bus |

| Campo | Por defecto | Qué |
|---|---|---|
| `root` | `""` | la carpeta, absoluta. Con valor, tiene que existir al arrancar (si no, la ejecución falla con el motivo). Vacía: espera a la entrada `root` |
| `tools` | las ocho | las operaciones que este workspace tiene. La que falta no se ofrece a nadie: ninguna vista la devuelve |
| `protect` | `[".git"]` | nombres en los que no entra ninguna operación de ficheros, a cualquier profundidad y sin mirar mayúsculas |
| `skip` | `["node_modules", "target"]` | carpetas en las que una búsqueda o un listado profundo no bajan. Nombrarlas en `path` sí funciona |
| `max_read_bytes` · `max_list_entries` · `max_search_matches` · `max_diff_bytes` · `max_output_bytes` | 16000 · 300 · 60 · 16000 · 6000 | lo que una llamada devuelve a un agente. Caben en los 20000 caracteres que el motor nativo enseña de un resultado (`observation_max_chars`): un fichero largo se lee por trozos |
| `max_line_chars` | 200 | caracteres de una línea que coincide |
| `max_search_files` · `max_search_file_bytes` · `search_timeout_ms` · `max_search_depth` | 20000 · 1 MiB · 5000 · 64 | dónde para una búsqueda: ficheros mirados, tamaño a partir del cual un fichero se salta, tiempo (se mira en cada carpeta y en cada fichero) y niveles de carpetas por debajo de donde empieza. Parar en cualquiera, o dejar algo sin mirar, responde `truncated` |
| `max_folder_entries` | 20000 | entradas de una misma carpeta que mira un listado o una búsqueda; una carpeta con más se lee en parte y responde `truncated` |
| `max_write_bytes` | 1 MiB | el fichero más grande que se escribe o se edita |
| `inspect_max_bytes` · `inspect_max_entries` | 1 MiB · 2000 | lo que `code.inspect` da a un cliente: son otros límites que los del agente |
| `shell` | `["sh", "-c"]` | programa y argumentos que ejecutan una línea; la línea va como último argumento |
| `command_timeout_ms` · `max_command_timeout_ms` · `kill_grace_ms` · `pipe_grace_ms` · `max_running` | 120000 · 600000 · 2000 · 500 · 4 | el plazo si la llamada no dice otro, el máximo que puede pedir, el margen entre SIGTERM y SIGKILL, lo que espera a que lo dejado en segundo plano suelte la salida, y cuántos comandos a la vez (0: sin límite) |
| `env_passthrough` | `PATH HOME LANG LC_ALL TERM USER LOGNAME SHELL TMPDIR` | las variables del core que hereda un comando. **Nada más le llega** |
| `env` | `{}` | variables extra; un valor puede ser `secret:<nombre>` |
| `git` · `git_timeout_ms` | `git` · 15000 | el programa git y su plazo por ejecución |
| `git_dir` | `any` | `inside`: el directorio del repo tiene que estar bajo la raíz (un worktree enlazado o un submódulo lo tienen fuera y quedan sin git). Ver §7 |

### 2.2 `tools` — una parte del workspace

Entrada "usa" `workspace` (`code.workspace@1`, obligatoria) y salida "usa" `tools`
(`tool@1`). Campos: `access` (`read`: lo que no cambia nada; `write`: además escribir y
editar; `all`: además la shell; por defecto `read`) y `tools` (de eso, solo esos nombres;
vacío: todo). **Solo puede estrechar.** Es lo que recibe un trabajador de solo lectura:
las herramientas que cambian algo ni siquiera se le enseñan. La vista decide por lo que
la operación *hace* (`access`), no por su nombre, así que vale para cualquier workspace.

### 2.3 `instructions` — las instrucciones del repositorio

Entrada "usa" `workspace` (obligatoria) y salida "usa" `hook` (`hook@1`). En su `event`
(`session_start` por defecto, o `prompt`) responde con los ficheros de `files`
(`AGENTS.md`, `CLAUDE.md`) que existan, cada uno bajo `header` (`{{path}}` es el
fichero), hasta `max_chars` (24000) en total. Los lee **como texto, a través del
workspace**: dentro de la raíz, con sus límites, y sin ejecutar nada del repositorio. Un
fichero que se alcanza por dos nombres (`CLAUDE.md` enlazado a `AGENTS.md`) sale una vez;
uno que falta, es una carpeta, está fuera o no es texto, simplemente no es una
instrucción. Sin ninguno, responde "nada". Se cablea en `context` del motor nativo; por
ser un `hook@1` también entra en `hooks` de la caja Agente, pero eso no se ha probado con
el motor de un CLI.

---

## 3. Las operaciones, como herramientas (`tool@1`)

El contrato `tool@1` se usa tal cual (`crates/contracts/tool`): `tool.list` → `[Spec]`,
`tool.call` con `{name, args, caller}` → `{data, error?}`. **Una operación rechazada o
que falla es una respuesta** (`error: true` y el motivo como texto en `data`), nunca una
llamada rota: el agente la lee y decide.

| Herramienta | Argumentos | Devuelve |
|---|---|---|
| `code_list_files` | `path?`, `depth?` (1) | `{path, entries: [{path, kind, size?}], truncated}`, por ruta |
| `code_read_file` | `path`, `offset?` (línea, desde 1), `limit?` (líneas) | `{path, text, start_line, end_line, size, truncated}`: líneas enteras hasta `max_read_bytes`; con `truncated`, se sigue en `end_line + 1`. Una sola línea más larga que el tope llega cortada |
| `code_search` | `query`, `path?`, `glob?`, `regex?`, `ignore_case?` | `{matches: [{path, line, text}], truncated}`. `query` es el texto exacto salvo `regex` |
| `code_git_status` | — | `{repository: true, branch, head, upstream, ahead, behind, status, truncated}` o `{repository: false, reason}` |
| `code_git_diff` | `path?` | `{repository: true, text, truncated}` o `{repository: false, reason}` |
| `code_write_file` | `path`, `content` | `{path, bytes, created}`. Crea las carpetas. Escribe al lado y renombra: nunca se ve medio fichero, y conserva los permisos del que había |
| `code_edit_file` | `path`, `old_text`, `new_text`, `replace_all?` | `{path, replacements, bytes}` |
| `code_run_command` | `command`, `cwd?`, `timeout_ms?` | `{command, cwd, code, signal, timed_out, ms, stdout, stderr, truncated}`; `error: true` si no salió con 0 |

**Conflictos de edición.** `old_text` tiene que estar en el fichero exactamente (espacios
y finales de línea incluidos) y una sola vez. Si no está: `conflict: `old_text` is not
in…` (el agente leyó una versión vieja). Si está varias veces sin `replace_all`:
`conflict: … N times`. En los dos casos el fichero no se toca. Es la detección de
conflictos: una edición hecha desde una lectura caducada no cae en otro sitio.

**Dos cambios a la vez.** Escribir y editar toman un cerrojo de todo el proceso, de la
lectura al renombrado: dos ediciones simultáneas del mismo fichero (el agente y un
trabajador con permiso, dos nodos sobre la misma carpeta) entran las dos. Se suelta antes
de emitir `changed`. **Un fichero sin permiso de escritura no se cambia**, aunque su
carpeta lo permitiría (se sustituye renombrando).

**Comandos.** La línea va a `shell`, en la raíz o en `cwd` (una carpeta de la raíz), con
el entorno **vacío** más `env_passthrough` y `env`. stdin cerrado. De stdout y de stderr
se guarda `max_output_bytes` de cada uno: **el principio y el final**, con una línea que
dice cuánto se cortó (un build dice qué falló al final). Pasado el plazo, SIGTERM al
grupo y SIGKILL tras `kill_grace_ms`. **Al acabar, bien o mal, se mata lo que quede en
su grupo:** una herramienta no puede dejar un servidor levantado (para eso está el `job`
de `process`). Parar la ejecución (`node/stop`) mata los comandos en curso al momento.

**Argumentos tolerantes:** un número puede llegar como texto (`"offset": "12"`) y un
booleano como `"true"`: los modelos escriben las dos cosas.

---

## 4. La capacidad `code.workspace@1`

🔶 Vive en el módulo (`regulus_module_code::contract`), no en `crates/contracts`: hoy es
su único servidor y su único consumidor. Se mueve allí cuando un segundo módulo la sirva
o la consuma. Tres métodos, todos con `NodeCall`:

| Método | Args | Responde |
|---|---|---|
| `code.workspace.describe` | — | `{root, operations: [{name, description, input, access}]}`; `access` es `read`, `write` o `execute` |
| `code.workspace.call` | un `Call` de `tool@1` | un `Outcome` de `tool@1` |
| `code.workspace.inspect` | `{action, path?, query?}` | lo mismo que `code.inspect` (§5) |

Con esto un workspace es una caja líquida: otro módulo puede ofrecer la misma capacidad
(una carpeta remota, un contenedor) y la vista `tools`, `instructions` y cualquier caja
nueva siguen funcionando sin tocar nada. Cuando el workspace cableado es de este mismo
módulo, la vista lo llama directamente (sin pasar por el core) para no reentrar en su
propio módulo.

---

## 5. `code.inspect` — la mirada de solo lectura

Método global del módulo, para los clientes (CLI, app):
`{execution_id, node_id, action, path?, query?}`. `node_id` es un nodo `workspace` de
este módulo, vivo en esa ejecución. No cambia nada.

| `action` | Responde |
|---|---|
| `summary` | `{root, git, tools, git_reason?}`. `git` es **`null`** si git no puede leer la carpeta, y entonces `git_reason` dice por qué (`not a git repository`, git no instalado, repo de otro usuario, plazo). Si puede: `{branch, status, head, upstream?, ahead?, behind?, changes, truncated}`: `branch` es `null` con HEAD suelto; `head` es `null` antes del primer commit; `status` es el texto de `git status --porcelain=v1` del subárbol de la raíz, con rutas relativas a ella (`XY ruta` por línea, `XY viejo -> nuevo` en un renombrado, vacío si está limpio); `changes` son esas líneas ya leídas (`[{path, code, from?}]`) |
| `list` | `{root, path, entries: [{path, kind: "file"\|"directory", size?}], truncated}`. Los hijos directos de `path` (por defecto la raíz), rutas relativas a la raíz, por orden de ruta. Un enlace sale con el tipo de aquello a lo que lleva **si eso está dentro**; si no, no sale. Tampoco salen sockets, dispositivos ni nombres que no son UTF-8 |
| `read` | `{root, path, text, truncated, size}`. Desde el principio, UTF-8, hasta `inspect_max_bytes`; un carácter partido por el corte se quita |
| `search` | `{root, matches: [{path, line, text}], truncated}`. `query` **literal y sin mirar mayúsculas** (es para una persona); `path` lo acota a una carpeta o a un fichero. No sigue enlaces; salta binarios, ficheros de más de `max_search_file_bytes` y las carpetas de `skip` |
| `diff` | `{root, text, truncated, git_reason?}`. Los cambios rastreados contra `HEAD`, en el índice o no, como un diff unificado; `path` lo acota. **Los ficheros sin rastrear no están**: son las líneas `??` del `status`. Antes del primer commit, contra el árbol vacío. Fuera de un repo, `text` vacío y `git_reason`, no un error |

**Errores:**

| Código | Cuándo |
|---|---|
| `-32602` | petición mala: `action` desconocida o ausente, `read` sin `path`, `search` sin `query`, ejecución o nodo que no es un workspace vivo de este módulo, ruta fuera de la raíz, a través de un enlace roto, protegida, inexistente o del tipo equivocado (`read` de una carpeta, `list` de un fichero), fichero binario o que no es UTF-8 en `read` |
| `-32003` | falló el workspace: sin raíz todavía, la carpeta ya no existe, error de disco |
| `-32601` · `-32002` | los del core, como con cualquier módulo: no instalado · inutilizable |

Los trabajadores no tienen workspace propio: usan el del padre. Un cliente mira con el id
de la ejecución padre.

---

## 6. La plantilla `regulus-code`

`templates/regulus-code.toml`. Workflow principal `code`; nodos `agent` (la conversación),
`native` (el bucle), `provider`, `workspace`, `instructions`, `decider`, `workers`,
`worker_engine`, `reader`; workflow `worker` (una caja Agente por trabajador). Se edita en
el canvas como cualquier perfil.

| Hueco | Por defecto | |
|---|---|---|
| `root` | — (obligatorio) | la carpeta, absoluta |
| `label` | `Regulus Code` | el nombre del perfil |
| `preset` · `base_url` · `credential` · `model` | `ollama` · `""` · `""` · `qwen3:4b` | el proveedor |
| `system` · `worker_system` | — | los prompts de sistema |
| `tool_policy` · `allow_tools` | `ask` · las cinco lecturas y `worker_status`, `worker_result`, `worker_list`, `worker_close` | la política |
| `worker_access` · `worker_allow_tools` · `worker_permissions` | `read` · las cinco lecturas · `reject` | los trabajadores |
| `max_workers` · `max_steps` · `max_tool_calls` · `history_turns` · `max_tokens` | 2 · 24 · 48 · 20 · 4096 | límites del bucle |
| `request_timeout_ms` · `turn_timeout_ms` · `decision_timeout_ms` | 120000 · 0 · 600000 | plazos |
| `observation_max_chars` · `max_read_bytes` · `max_output_bytes` · `command_timeout_ms` | 20000 · 16000 · 6000 · 120000 | lo que ve el modelo |
| `instructions` | `AGENTS.md,CLAUDE.md` | los ficheros de instrucciones |

**Quién puede qué.** Leer no pregunta. Escribir, editar, ejecutar un comando, crear un
trabajador o escribirle **esperan a una persona**: el motor emite `permission_request`
(con la herramienta y sus argumentos en el título: la persona ve la ruta o el comando), la
caja Agente la lleva al Decisor y se contesta con `regulus human answer <id> approve
--field option=allow_once` (o `allow_always`, que vale para el resto de la sesión, o
`reject_once`). Sin respuesta en `decision_timeout_ms`, se rechaza. Un rechazo le llega al
modelo como `refused: …` y el turno sigue.

**Trabajadores.** `worker_create` arranca una ejecución hija viva (`ChildMode::Keep`) del
workflow `worker`, sobre `worker_engine`: mismo modelo, mismas instrucciones, y la
carpeta **a través de `reader`**. Con `worker_access = read` un trabajador solo ve las
cinco lecturas. No son los subagentes de un CLI (`12-agents.md` §5.1).

🔶 **Permisos de un trabajador.** El nodo `workers` declara una entrada "usa" `decisions`
propia (el Delegado pasa a sus trabajadores cualquier entrada "usa" que el nodo declare) y
el workflow `worker` la recibe. Con `worker_permissions = ask` y un `worker_access` más
ancho, la petición de un trabajador llega **a la misma persona**, a nombre del
trabajador. Probado.

**Lo que ve un cliente.** La conversación es la de la caja Agente (`agent.send`,
`agent.history`, `agent.cancel`): texto, cada llamada a herramienta con su estado, cada
petición de permiso y su respuesta. Los trabajadores salen en `execution.tree` y cada uno
tiene su propia conversación. Lo que el workspace cambia y ejecuta sale además por
`self.changed` y `self.ran`.

---

## 7. Seguridad: lo que se garantiza y lo que no

**Se garantiza** (y hay un test por cada cosa, §9): contención de rutas con enlaces
resueltos, nombres protegidos, ediciones solo con coincidencia exacta, plazo y limpieza de
procesos, entorno vacío para los comandos, git sin programas del repositorio.

**No se garantiza:**

- **La shell no aísla nada.** Un comando aprobado puede leer `~/.ssh`, borrar fuera de la
  raíz o abrir la red. Aislar la ejecución está fuera de v1 (`11-composition.md` §12).
- **La contención se comprueba antes de operar, no durante.** Un programa que cambie una
  carpeta por un enlace entre la comprobación y la escritura no está cubierto (haría falta
  `openat2`), ni un enlace duro a un fichero de fuera.
- **Un editor de fuera no entra en el cerrojo.** Si otro programa escribe el mismo fichero
  mientras el módulo lo edita, gana el último. Lo que sí se detecta es que el texto ya no
  coincida.
- **Los nombres protegidos se comparan como texto**, sin mirar mayúsculas. Un sistema de
  ficheros que iguale nombres de otra forma (HFS+ con caracteres invisibles) no está
  cubierto.
- **Un comando en curso no se cancela con el turno.** `agent.cancel` corta el turno cuando
  la herramienta vuelve; el comando acaba en su plazo o al parar la ejecución.
- **Lo que se escapa del grupo de procesos** (`setsid`) no se mata; su salida se abandona.
- **Inyección de instrucciones.** Las instrucciones del repo, cualquier fichero leído y la
  salida de un comando son texto que otro pudo escribir y que el modelo puede obedecer. Lo
  que lo frena es que los cambios preguntan.
- **git lee el repo al que la carpeta dice pertenecer.** Un `.git` que sea un fichero
  puede apuntar a otro repositorio del disco y un diff enseñaría su contenido.
  `git_dir = inside` lo cierra, a costa de los worktrees enlazados. Un repo cuyo
  `core.worktree` apunta a otra carpeta se rechaza siempre.
- **Sin límite de disco:** `max_write_bytes` acota cada fichero, no la suma.

**"`inspect` no invoca comandos definidos por el usuario"** quiere decir: los ficheros
los lee el propio módulo, y el único programa que corre es el `git` del campo `git`, con
todo lo que una configuración le puede hacer ejecutar apagado. No es "ningún
subproceso".

---

## 8. Medido

2026-11-10, sobre este repositorio (552 ficheros, un worktree enlazado), mediana de 7,
llamando a `code.inspect` dentro del proceso.

| Acción | Debug | Release |
|---|---|---|
| `summary` (3 procesos git) | 11,6 ms | 11,6 ms |
| `list` (20 entradas) | 0,24 ms | 0,10 ms |
| `read` (16 KB · 192 KB) | 0,12 · 0,21 ms | 0,05 · 0,13 ms |
| `search` (todo el árbol, 288 coincidencias) | 357 ms | 31 ms |
| `diff` (4 procesos git) | 11,6 ms | 11,1 ms |

`list` y `read` entran en el presupuesto de lectura de `00-principles.md` (< 5 ms).
`summary` y `diff` **no**, y no mejoran en release: el tiempo es de git, que se lanza
varias veces (~3,5 ms por proceso). No se guarda nada entre llamadas a propósito: lo que
se guardaría es la lista de filtros apagados, y una configuración cambiada entre dos
llamadas la dejaría vieja. `search` es trabajo proporcional al árbol, no una lectura: lo
acotan `max_search_files`, `max_search_depth`, `max_folder_entries` y `search_timeout_ms`.
El core arranca en 47 ms con el módulo (debug; presupuesto 200 ms).

---

## 9. Probado

Sin modelo real en ningún caso.

- **Unidad** (`crates/modules/code/src`, 24): rutas (raíz, `..`, enlaces dentro, fuera y
  rotos, nombres protegidos), lectura por líneas acotada, UTF-8 y binarios, captura con
  principio y final, entorno vacío, lectura de `git status`, una búsqueda sin tiempo en un
  árbol de solo carpetas, el corte por profundidad y por tamaño de carpeta, y que **el
  manifiesto declara exactamente los campos y puertos que el código lee**.
- **Contra un core real** (`crates/modules/code/tests/code.rs`, 19): listar, leer por
  trozos y buscar con sus topes; ninguna operación sale de la raíz ni entra en `.git`;
  escritura atómica, conflictos de edición y un fichero de solo lectura intacto; **doce
  ediciones a la vez sobre un fichero, por dos nodos, entran las doce** (sin el cerrojo se
  pierde una en la primera ronda); un comando con plazo mata también lo que dejó
  en segundo plano, y parar la ejecución mata el que está en curso; `git status` y `diff`
  en la raíz, en una subcarpeta, sin commits, con HEAD suelto, sin repo y sin git; **un
  repo con fsmonitor, hook, filtros, diff externo, textconv y pagers configurados no
  ejecuta ninguno a través del módulo, después de comprobar que con un git normal sí**;
  `core.worktree` y `git_dir = inside`; las formas y los códigos de `code.inspect`; las
  vistas; las instrucciones; la entrada `root`, y que una raíz movida sigue movida tras
  reiniciar el core sobre un store real; y la vista y las instrucciones sobre **un módulo
  falso** que ofrece `code.workspace@1`.
- **La plantilla, por el programa real** (`crates/regulus-cli/tests/
  template_regulus_code.rs`, 4): aplica y pasa `profile.check` sin diagnósticos; cada
  campo y cada puerto de sus nodos está en el manifiesto de su módulo; con un modelo
  falso, un turno lee sin preguntar, edita y ejecuta tras aprobar, se le rechaza otro
  comando y delega en un trabajador de solo lectura; `allow_always` vale para la sesión;
  un trabajador con permiso para preguntar llega a la misma persona; y sin el módulo la
  plantilla dice qué falta y lo demás sigue funcionando.
- **Ablación:** la matriz es genérica sobre `installed()`, así que cubre el módulo desde
  que se registró (`crates/regulus-cli/tests/ablation.rs`).

---

## 10. Decidido al construir (🔶) y límites

| | Qué | Por qué |
|---|---|---|
| 🔶 | `git: null` + `git_reason` como hermano | el coordinador pidió `null`; un `null` no puede llevar el motivo dentro |
| 🔶 | `diff` sin ficheros sin rastrear | decisión del coordinador; salen en `status` |
| 🔶 | `search` de `inspect` sin mirar mayúsculas, el de la herramienta exacto | uno es para una persona, el otro para un modelo que puede pedir `ignore_case` |
| 🔶 | Dos juegos de límites (agente · cliente) | un visor quiere el fichero entero; un modelo, lo que cabe en su contexto |
| 🔶 | Nodos `tools` e `instructions`, entrada `root`, salidas `changed`/`ran` | el contrato pedía trabajadores de solo lectura, instrucciones por hook y un nodo reutilizable |
| 🔶 | Crear un trabajador pregunta | cuesta llamadas al modelo; con `allow_always` es una vez por sesión |
| Límite | Solo UTF-8: un fichero en otra codificación no se lee ni se edita | editar con pérdida sería peor |
| Límite | `code_run_command` necesita Unix (grupos de procesos); en otro sistema responde un error | — |
| Límite | Una ruta con salto de línea en su nombre deja a git sin leer | — |
| Límite | Dos workspaces en el mismo motor repiten nombres de herramienta: el motor se queda con el primero. Uno por motor, o estrecharlos con `tools` | no hay prefijo de nombres |
| Límite | `instructions` solo lee los ficheros que nombra: no busca un `AGENTS.md` por subcarpeta | — |

**Lo que este módulo no es:** no hay streaming, compactación de contexto, checkpoints ni
rebobinado, y el historial que se guarda es el de la caja Agente, no el contexto del
modelo (`12-agents.md` §5, §8). No se promete paridad con Claude Code.

---

## 11. Reutilizarlo en otro workflow

Composiciones que el diseño permite. **Probadas en este módulo, solo dos:** la vista de
solo lectura y la entrada `root` (con un valor enviado a mano). Las demás salen de
contratos que ya existen (`path@1`, `hook@1`, `tool@1`) y están sin correr.

- **Un worktree por ejecución:** `workspace` (módulo `workspace`, motor git) ─`ready`→
  `root` del workspace de código. Cada ejecución programa en su rama.
- **Un revisor que no puede tocar nada:** workspace ⇢ `tools` (`access = read`) ⇢ motor.
- **Vetar por regla:** un `rule` de `hooks` en `context` del motor, en `pre_tool`, con
  matcher `code_run_command`.
- **Seguir hasta que pasen los tests:** un `gate` de `process` detrás de `changed`, o un
  hook `stop`.
- **Otro agente:** las herramientas llegan a Claude Code u opencode por una caja `tools` ⇢
  `tools-mcp`; ahí los permisos son los de la caja Agente.
