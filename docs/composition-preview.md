# Incremento visual de composición — A'/B'

> 2026-10-04. Implementado por petición de Alexander. Prototipo de solo lectura;
> no cierra los contratos abiertos de `11-composition.md` ni implementa el core.

## Qué se puede comprobar

`pnpm dev` desde `crates/regulus-tauri/frontend` arranca Vite y el mock en la misma
conexión local. En Composición se puede ver el workflow principal, uno extra y el interno
del módulo de comunicación; verlos juntos; seleccionar nodos y examinar puertos,
referencias a secretos y parámetros expuestos; abrir el workflow interno de un nodo.
Zoom y cámara son estado del cliente. No se crean nodos, cables ni ejecuciones.

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
| `profile.check` *(1.1, solo core real)* | `id` | Diagnósticos del perfil: `severity`, `code`, `message` |
| `execution.start` *(1.2)* | `profile_id`, `workflow_id?` | La ejecución: `id`, `state`, y `disabled` (nodos sin sus entradas obligatorias) |
| `execution.stop` *(1.2)* | `id` | La ejecución parada |
| `secret.set` *(1.3)* | `name`, `value` | `null` |
| `secret.list` *(1.3)* | — | Nombres de los secretos, nunca los valores |
| `secret.delete` *(1.3)* | `name` | `null` |

El core real (`regulus serve`) habla el protocolo `1.3`, compatible con el `1.0` del mock.
Los métodos de los módulos se suman a estos (p. ej. `agent.prompt`, `claude.detect`).
Errores propios: `-32001` falta `hello`, `-32002` módulo no disponible, `-32003` el módulo
falló o entró en pánico. Además pide el token de `<home>/token` en el upgrade
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
