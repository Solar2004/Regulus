# Incremento visual de composición — A'/B'

> 2026-10-04. Implementado por petición de Alexander. Prototipo de solo lectura;
> no cierra los contratos abiertos de `11-composition.md` ni implementa el core.

## Qué se puede comprobar

`pnpm dev` desde `crates/regulus-tauri/frontend` arranca Vite y el mock en la misma
conexión local. En Composición se puede ver el workflow principal, uno extra y el interno
del módulo de comunicación; verlos juntos; seleccionar nodos y examinar puertos,
referencias a secretos y parámetros expuestos; abrir el workflow interno de un nodo.
Zoom y cámara son estado del cliente. Desde el 2026-11-08 el canvas edita: crea perfiles,
nodos y cables, guarda con `profile.save` y arranca ejecuciones con `execution.start`
([`04-client.md`](04-client.md) §9).

El perfil de ejemplo vive en `frontend/mock/profile.json`. No representa módulos
instalados de verdad. Se pueden cambiar nodos, coordenadas y workflows sin tocar el
renderizador. El shell conserva sus estados vacíos cuando el servidor no responde.

## Contrato provisional del mock

JSON-RPC sobre WebSocket, ruta de desarrollo configurada en `frontend/mock/config.json`.
Primero `hello`, obligatorio por conexión. El servidor solo permite lectura después de
negociar un major compatible.

| Método | Parámetros | Estado devuelto |
|---|---|---|
| `hello` | `client`, `proto` | `core`, `proto`, `compatible` |
| `profile.list` | — | Resúmenes con `id`, `name`, `project_id` |
| `profile.get` | `id` | Perfil con workflow principal y todos sus workflows |
| `module.list` | — | Módulos de ejemplo y tipos de nodo que aportan |
| `execution.list` | — | Lista vacía; nada está ejecutándose |
| `profile.check` *(1.1)* | `id` | Diagnósticos del perfil: `severity`, `code`, `message` |
| `execution.start` *(1.2)* | `profile_id`, `workflow_id?` | La ejecución: `id`, `profile_id` *(1.4)*, `workflow_id`, `state`, y `disabled` (nodos sin sus entradas obligatorias) |
| `execution.stop` *(1.2)* | `id` | La ejecución parada |
| `secret.set` *(1.3)* | `name`, `value` | `null` |
| `secret.list` *(1.3)* | — | Nombres de los secretos, nunca los valores |
| `secret.delete` *(1.3)* | `name` | `null` |
| `event.list` *(1.5)* | `since?`, `execution_id?`, `project_id?`, `limit?` | `events` del bus (`emitted`, `delivered`, `failed`, `cut`), `next` (cursor) y `pending` |
| `workflow.send` *(1.7)* | `execution_id`, `port`, `value` | `event`: el `seq` del `emitted`; la respuesta sale en `event.list` desde `self.<salida>` |
| `hook.dispatch` *(1.9)* | `execution_id`, `node_id`, `hook_id?`, `timeout_ms?`, `returns?`, `request` (`HookRequest`) | `replies`: por cada nodo `hook@1` conectado a `node_id`, `node_id`, `answer` (`decision`: `none` \| `veto` + `reason` \| `context` + `text`) y `error?` |
| `template.list` *(1.10)* | — | Plantillas de `[templates] paths` y de cada proyecto abierto: `name`, `description`, `path`, `vars` (`name`, `description`, `default?`), `requires` (`module`, `version`), `error?` si el fichero no se puede leer como plantilla y `project_id?` *(1.12)* |
| `template.get` *(1.10)* | `template` (nombre o ruta) | `name`, `path` y `text`: el fichero tal cual |
| `template.apply` *(1.10)* | `template`, `vars` (`{nombre: valor}`), `id?`, `force?`, `project_id?` *(1.12)* | `profile_id`, `path` (el fichero escrito) y `diagnostics` (avisos e información; un error de `profile.check` impide escribir) |
| `module.manifest` *(1.11)* | `id?` (con o sin versión) | El `Manifest` del módulo con los nombres del TOML (`module`, `node[]` con `type`, `name`, `port[]`, `field[]`: `id`, `type`, `default`, `options`); sin `id`, la lista de todos. También de un módulo `broken` o `incompatible` |
| `profile.save` *(1.11)* | `profile` (el perfil entero, como lo da `profile.get`), `force?` | `profile_id`, `path` y `diagnostics`, como `template.apply` |
| `profile.delete` *(1.11)* | `id` | `profile_id` y `path` (el fichero borrado) |
| `project.list` *(1.12)* | — | Proyectos abiertos: `id`, `name`, `path` (la carpeta), `warnings` y `error?`: una carpeta de `[projects] paths` que no abrió al arrancar (`regulus.toml` ilegible o no TOML, id inválido, carpeta que no está) sale con `error` y nada suyo cargado; `project.open` lo reintenta y `project.close` la olvida |
| `project.get` *(1.12)* | `id` | El `regulus.toml` resuelto: `id`, `name`, `path`, `file`, `profiles` y `templates` (rutas absolutas), `disable`, `enable?`, `local` (tal cual, sin cargar), `client` (tabla libre), `content` (el fichero entero con las claves desconocidas) y `warnings` |
| `project.open` *(1.12)* | `path` (carpeta o su `regulus.toml`; relativa al home del core) | Como una entrada de `project.list`. Abrir una carpeta ya abierta no cambia nada |
| `project.close` *(1.12)* | `id` | Como una entrada de `project.list`; no borra nada en disco |
| `config.client` *(1.13)* | `project_id?` | `client`: el `[client]` de `config.toml` tal como está escrito, con el del proyecto encima clave a clave (las tablas como `keys` y `tui`, también clave a clave; lo desconocido pasa tal cual); `sources`: de qué fichero sale cada valor (`config.toml` o `regulus.toml`, por `clave` o `tabla.clave`); `warnings`: las claves de `[client]` que el core no conoce. Proyecto que no está abierto: `-32010`. Detalle en [`04-client.md`](04-client.md) §11 |

El core real (`regulus serve`) habla el protocolo `1.13`, compatible con el `1.0` del mock.
Desde `1.12` hay proyectos ([`11-composition.md`](11-composition.md) §13): `project.open`
añade la carpeta a `[projects] paths` de `config.toml` y suma sus perfiles y plantillas;
`profile.save` de un perfil con `project_id` de un proyecto abierto y `template.apply` con
`project_id` escriben en la carpeta de perfiles del proyecto; `profile.check` marca como
`node.module_disabled` (aviso, "disabled in project <id>") los nodos de un módulo vetado en
su proyecto y `execution.start` no los arranca.
Desde `1.11` la app puede editar: `module.manifest` da los tipos de nodo con puertos, campos,
defaults y opciones; `profile.save` escribe el perfil entero como `<id>.toml` en la primera
ruta de `[profiles] paths` y lo carga sin reiniciar (nuevo sin `force`; uno cargado, solo con
`force` y si su fichero está ahí y lo tiene a él solo; si vive en otro sitio, el error dice
dónde); `profile.delete` borra el fichero si está en una carpeta de `[profiles] paths` y solo
tiene ese perfil, y se niega mientras una ejecución suya arranca, corre o espera a reanudarse.
El fichero se reescribe entero: los comentarios se pierden. El mock los imita en memoria
(`mock/manifests.json`, sacado de `flow` y `files` reales).
Desde `1.10` el core aplica plantillas (`11-composition.md` §7): escribe la copia en la
primera ruta de `[profiles] paths` y la carga sin reiniciar; nunca pisa un perfil sin `force`.
Desde `1.9` el core reenvía las preguntas de los hooks de un agente (`13-blocks.md` §4);
lo usa el puente `regulus hook`, no la UI.
Desde `1.8` una ejecución puede traer `parent`: la arrancó un nodo de otra
(`02-architecture.md` §4 regla 5).
Desde `1.7` un workflow puede traer `ports` (sus entradas y salidas; los cables de dentro
las nombran `self`).
Los métodos de los módulos se suman a estos (p. ej. `agent.prompt`, `claude.detect`).
Errores propios: `-32001` falta `hello`, `-32002` módulo no disponible, `-32003` el módulo
falló o entró en pánico; de plantillas *(1.10)*: `-32004` no existe, `-32005` mal formada
(hueco sin declarar, no da un perfil, el perfil tiene errores), `-32006` falta un módulo de
`requires` o su versión no encaja, `-32007` el perfil ya existe; una var sin valor es `-32602`;
de perfiles *(1.11)*: `-32007` también si el perfil vive donde no se puede pisar ni borrar
(otro fichero, un fichero con más perfiles, ninguno), `-32008` el perfil que se guarda tiene
errores de `profile.check`, `-32009` el perfil que se borra está en uso; un id que no puede ser
nombre de fichero o un `null` en un `config` (TOML no lo tiene) es `-32602`; de proyectos
*(1.12)*: `-32010` no hay proyecto abierto con ese id o la ruta no es una carpeta, `-32011` el
proyecto no abre: su `regulus.toml` no se lee o no es TOML (el error de TOML va en el mensaje)
o su id no vale como nombre de fichero, `-32012` otro proyecto abierto tiene ese id, `-32013`
`execution.start` rechaza un nodo cuyo módulo está vetado en el proyecto; no poder escribir
`config.toml` es `-32003`. Además pide el token de `<home>/token` en el upgrade
(`?token=` desde la webview).

El perfil contiene `project_id` nullable y `extends` reservado. Cada workflow contiene
sus nodos, cables y coordenadas. Cada nodo declara módulo o recurso, tipo, configuración,
puertos y una referencia opcional a su workflow interno. Cada cable une ids de nodo y
puerto, y distingue `uses` (discontinuo) de `then` (continuo). No se infieren ejecuciones
a partir del dibujo. El fixture solo declara referencias `secret:...`, nunca claves.

Los tipos del cliente viven en `frontend/src/composition/model.ts`. El servidor sirve
el JSON sin tipos ni componentes de React Flow. Esto es un contrato provisional para
examinar el modelo; aún faltan validación general de manifiestos y los contratos de
payload de cada capacidad. El fixture verifica puertos compatibles, pero no ejecuta
servicios ni eventos.

## Configuración y límites

- `VITE_RPC_URL` permite conectar el cliente a otro endpoint compatible. Sin esa variable
  se usa el host del cliente y `VITE_RPC_PATH` (por defecto la ruta del mock).
- El mock es solo de desarrollo. Un build de producción no lo incluye; necesita un
  endpoint real o muestra el estado de desconexión con reintento.
- No hay autenticación ni secretos reales en este mock. No sustituye el token del core.
- Los permisos y el aislamiento de ejecución no están implementados.
- Parámetros expuestos se muestran, pero no se editan en B'.
- No se ha medido todavía el presupuesto de primera pantalla ni la latencia del canvas.

Verificación: `pnpm build`, `pnpm test:mock` y navegación en el navegador. Las versiones
de dependencias viven solo en [`06-stack.md`](06-stack.md).
