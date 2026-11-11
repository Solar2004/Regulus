# 17 — Control MCP: gobernar Regulus desde un cliente MCP

> **v1.0** · 2026-11-10 · ✅ *Construido y probado* con un cliente MCP de prueba contra un
> core real. **No se ha probado con un cliente comercial ni con un modelo real.**
> Código: `crates/regulus-cli/src/control_mcp/`. Pruebas:
> `crates/regulus-cli/tests/control_mcp.rs`, `bundled_templates.rs`.

---

## 1. Qué es

`regulus mcp` es un servidor MCP por stdio que **es un cliente más del core**: se conecta
al socket autenticado de `--home`, dice `hello` y convierte una lista fija de herramientas
en métodos del core. No tiene estado, no abre puerto, no es un demonio y no enseña nada
nuevo al core (no hay métodos nuevos). **El core valida; el servidor solo traduce y explica.**

```
 cliente MCP ──stdio──▶ regulus mcp ──ws + token──▶ core ──▶ módulos
 (Claude Code,          (tabla de 19              (valida, guarda,
  Cursor, …)             herramientas)              ejecuta)
```

El puente `tools-mcp` sigue igual: ata *herramientas de una caja Agente*. Este controla el
*host*. Comparten el protocolo (`initialize`, `ping`, `tools/call` y sus errores, de
`regulus_module_tools_mcp::answer`); `initialize` y `tools/list` los contesta este
servidor (revisión negociada, anotaciones).

## 2. Configurarlo

```bash
regulus --home ~/.regulus mcp --print-config
```

imprime la entrada lista para pegar, con el binario y `--home` **absolutos**
(`examples/mcp/regulus.mcp.json` es la plantilla):

```json
{ "mcpServers": { "regulus": { "command": "/ruta/absoluta/regulus",
                               "args": ["--home", "/ruta/absoluta/.regulus", "mcp"] } } }
```

Vale para Claude Code (`.mcp.json`), Claude Desktop, Cursor… Hace falta un core en marcha
(`regulus --home … serve`): el servidor **no lo arranca** (sería un demonio escondido); si no
está, cada herramienta contesta con el comando exacto para arrancarlo, y en cuanto el core
aparece (o se reinicia) la siguiente llamada lo encuentra.

| Opción | Qué |
|---|---|
| `--read-only` | solo se ofrecen las herramientas que leen; las demás se rechazan con la razón |
| `--deny-module ID` | repetible; ver §4 |
| `--max-result-bytes N` | corta un resultado más largo (262144) con una nota que dice cómo acotar |
| `--default-events N` | eventos de `event_list` si no se pide `limit` (100) |
| `--timeout S` (global) | espera por llamada al core |

## 3. Las herramientas

| Herramienta | Método del core | Acceso |
|---|---|---|
| `status` | `hello` + `module.list` + `execution.list` | lee |
| `module_list` · `module_manifest` | `module.list` · `module.manifest` | lee |
| `project_list` · `project_get` | `project.list` · `project.get` | lee |
| `profile_list` · `profile_get` · `profile_check` | `profile.list` · `.get` · `.check` | lee |
| `template_list` · `template_get` | `template.list` · `.get` (solo nombres) | lee |
| `execution_list` · `execution_tree` · `event_list` | `execution.list` · `.tree` · `event.list` | lee |
| `module_authoring_guide` | — (texto) | lee |
| `profile_save` | `profile.save` | escribe, destructiva (con `force`) |
| `template_apply` | `template.apply` | escribe, destructiva (con `force`) |
| `workflow_start` · `workflow_send` | `execution.start` · `workflow.send` | escribe, no destructiva |
| `workflow_stop` | `execution.stop` | escribe, destructiva |

Cada herramienta tiene esquema JSON (`inputSchema`). El servidor valida **solo el primer
nivel**: claves desconocidas o que faltan, tipo JSON (un entero acepta `5.0`) y `enum`
cuando el esquema lo declara; los defaults se rellenan. Lo de más adentro (un perfil entero,
`vars`) lo valida el core. Además y anotaciones MCP (`readOnlyHint`,
`destructiveHint`, `idempotentHint`, `openWorldHint` — los workflows pueden tocar la web o
modelos). **Las anotaciones son pistas para el cliente, no autorización**: lo que impide
algo es que la herramienta no exista (y `--read-only`).

## 4. El límite, dicho sin adornos

**Quien conecta con este servidor tiene la misma autoridad que quien tiene el token del
core local**, hasta donde lo permitan los módulos instalados. El servidor no añade ninguna
vía de escape, pero tampoco la cierra: sin `--read-only`, un cliente puede guardar un perfil
con un nodo `process` (un programa cualquiera) o `files` con `dir` absoluto o `allow_outside`,
arrancarlo con `workflow_start` y leer lo que emite con `event_list`. Y como el core resuelve
`secret:<nombre>` al arrancar un nodo, **un workflow puede usar secretos y mostrarlos**.
Lo que sí es verdad:

- No hay `call` genérico, ni `secret.*`, `hook.*`, `profile.delete`, `template.save`,
  `project.open` ni `project.trust`: ninguna herramienta los alcanza directamente. Tampoco
  los métodos de autoría del protocolo 1.17 (`source.get|save|check`, `node.init`,
  `project.reload`, `module.shape`, `profile.sync`): la tabla de herramientas es fija, así
  que ninguno esquiva `--read-only` ni `--deny-module`.
- **Confiar en el código de un proyecto es decisión de la persona** (`03-modules.md` §6):
  un módulo local de un proyecto sin confianza sale `blocked` y no arranca.
- `template_get`/`template_apply` aceptan **nombres**, no rutas.
- El **token** no sale en ningún resultado ni en stderr (se sustituye por `[redacted]`).
- El servidor no escribe ficheros ni lanza procesos *él*; lo hacen los módulos que un
  workflow use.

**Cómo acotar a un cliente poco fiable**, de más a menos fuerte:

1. `--read-only`: el único límite fuerte. Solo se ofrecen herramientas que leen.
2. **El veto por proyecto del core** (`[modules] disable` en `regulus.toml`,
   `11-composition.md` §13; `check_vetoed` en el core): `execution.start` rechaza los nodos
   de esos módulos para cualquier cliente.
3. `--deny-module <id>` (repetible, por id de manifiesto, con o sin `@versión`; vacío por
   defecto): `profile_save`, `template_apply` y `workflow_start` se niegan si el perfil
   (todos sus workflows, cajas incluidas) usa ese módulo. Es una guarda **de este servidor**
   para su cliente, no del core: otro cliente del mismo core no la tiene. `template_apply`
   mira el texto de la plantilla y se niega si el id lo decide una variable; `workflow_start`
   mira el perfil guardado, así que uno guardado antes no se puede arrancar desde aquí.
   Para un cliente no fiable:

   ```
   regulus --home … mcp --deny-module regulus/process --deny-module regulus/files
   ```

   (`--print-config` lo incluye solo si se pasa). No oculta nada más: un módulo que tú no
   hayas listado sigue accesible (p. ej. `regulus/web` o un módulo local).

## 5. Errores accionables

Una llamada que falla devuelve `isError: true` con el mensaje del core, su código
(`[core error -32008]`) y una pista de la herramienta (`hint:`). Tool desconocida: lista las
disponibles. Sin core: dónde arrancarlo. **No promete más validación que el core:**

| Caso | Qué hace el core de verdad |
|---|---|
| cable a un puerto que no existe, capacidades distintas, ids duplicados | **error**: `profile_save` rechaza y no escribe |
| nodo de un módulo no instalado | se guarda; `diagnostics` trae `node.module_not_installed` (info) y el nodo no corre |
| entrada obligatoria sin nada usable detrás | aviso `port.required_unwired`; la ejecución arranca y lista el nodo en `disabled`; `workflow_start` lo explica en `notes` |
| módulo `blocked` / roto / vetado | `workflow_start` falla con la razón del módulo |

`workflow_start` con estado `failed` es error de herramienta, con el motivo.

## 6. Añadir un módulo desde un cliente MCP

`module_authoring_guide` devuelve la receta (manifiesto, protocolo stdio, esqueleto Python,
la entrada `[[modules.local]]`). **Dos caminos reales**, ambos probados:

1. **Con herramientas de fichero propias** (un agente de código): escribe
   `tools/<id>/…` y la entrada en `regulus.toml`.
2. **Solo con este servidor:** la guía incluye un perfil de nodos `files.write` con `dir` =
   la carpeta del proyecto y sin `allow_outside` (con eso `files` mantiene lo escrito bajo
   `dir`; es la configuración de *ese* perfil, no una garantía del servidor). Se guarda con
   `profile_save`, se arranca y se le envían por `workflow_send` el manifiesto, el programa y
   el bloque `[[modules.local]]` (este último en modo `append`); `event_list` muestra las
   rutas escritas.

Luego **la persona** hace `project close` + `project open <carpeta> --trust` (las entradas
locales se leen al abrir). Hasta entonces `module_list` lo da `blocked`; después `idle`, y
`module_manifest`, `profile_save` y `workflow_start` lo comprueban. No existe `module install`:
un módulo Rust enlazado exige recompilar (`03-modules.md` §5); uno local, no.

## 7. Protocolo y ciclo de vida

- stdout lleva **solo** mensajes JSON-RPC, uno por línea; los diagnósticos van a stderr.
- `initialize` negocia: revisiones `2025-11-25`, `2025-06-18`, `2025-03-26`, `2024-11-05`; si
  el cliente pide otra (o nada) se contesta `2025-11-25`, nunca se repite una desconocida.
- Línea que no es JSON **o no es UTF-8 válido** → `-32700` y sigue. Un lote (array) →
  `-32600` "batches are not supported" (id `null`); un valor que no es objeto, o un mensaje
  con `id` sin `method` de texto → `-32600`. Método desconocido → `-32601`.
- `tools/call` sin `params.name` de texto → `-32602` (parámetros inválidos).
- Un fallo de herramienta es `isError: true`, **incluida una herramienta desconocida**:
  decisión consciente, el agente lee el motivo y la lista de las disponibles. La revisión
  2025-11-25 prefiere `-32602` para ese caso; no se sigue.
- **EOF en stdin = salida limpia (código 0).** Si el core cae, la herramienta contesta error;
  la conexión se rehace *antes* de la siguiente llamada, nunca se reenvía una llamada que
  pudo llegar (no se duplica un `execution.start`).
- Las llamadas se atienden en serie; los resultados largos se cortan (`--max-result-bytes`).

## 8. Las seis preguntas

1. Literales: topes y límites son opciones (`--max-result-bytes`, `--default-events`).
2. Ablación: no es módulo; es un cliente. Sin el core falla con un mensaje, no se rompe.
3. Rápido: una conexión reutilizada; sin estado propio.
4. Se cambia sin recompilar: el cliente configura el comando; la tabla de herramientas es del binario.
5. Infraestructura: es la ruta CLI→MCP de lo que ya hacen `regulus profile|template|run|send|stop|events`.
6. El core no conoce al servidor por nombre; no tiene métodos nuevos.

## 9. Pruebas y límites

`cargo test -p regulus-cli --test control_mcp` arranca el binario real como subproceso MCP
contra un core real en un home temporal (7 pruebas): protocolo y anotaciones, negociación de
revisión, `--print-config`, descubrir→crear→aplicar plantilla→arrancar→enviar→eventos→parar,
rechazos del core y errores accionables, `--read-only`, sin core / core reiniciado / EOF,
y la autoría de un módulo por `files.write` con confianza explícita (necesita `python3`).

Límites honestos: sin cliente comercial ni modelo reales; llamadas en serie sin cancelación
(`notifications/cancelled` se ignora); sin `structuredContent` (el resultado es JSON en
texto); las anotaciones son solo pistas.

## 10. Plantillas que se siembran

`regulus serve` copia las 17 plantillas de `templates/` que lleva el binario (la lista de
`crates/regulus-cli/src/bundled.rs`, que un test compara con la carpeta) a
`<home>/templates` como ficheros normales, **solo con el catálogo por defecto**. Cada nombre
se siembra **una vez**: queda anotado en `<home>/templates/.regulus-seeded`, así que una
plantilla que borras no vuelve, y una que editas no se pisa. **Las actualizaciones de una
plantilla incluida no se aplican a las copias existentes** (para recibirla, borra la copia y
la anotación de su nombre). Un fallo al escribir se avisa y no frena el core.
