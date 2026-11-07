# Changelog

Lo que cambia de cara a quien usa Regulus, agrupado por impacto. No es el `git log`: el
`git log` es para quien escribe el código, esto es para quien lo consume.

Formato basado en [Keep a Changelog](https://keepachangelog.com/es-ES/1.1.0/). Versionado:
[`docs/06-stack.md` §5](docs/06-stack.md) — tres ejes (app, protocolo, módulo).

**La entrada se escribe en el mismo commit que el cambio**, no al cortar el release
([`docs/09-git.md` §6](docs/09-git.md)).

---

## [No publicado]

### Añadido

- **Lógica de IA, el core** (`docs/13-blocks.md` §2, protocolo `1.5` → `1.8`): el cable
  "luego" lleva dato por un bus por ejecución (`event.list`, `regulus events --follow`,
  límites `[bus]`); un store SQLite global y por proyecto guarda ejecuciones, checkpoints
  y el diario del bus, y las ejecuciones sobreviven al reinicio de `serve` (`[store]
  resume`); un workflow declara puertos propios y se mete en otro como caja (`self.<puerto>`,
  `workflow.send`, `regulus send`); un nodo llama a un workflow como ejecución hija y
  espera su resultado (`Execution.parent`, `node/child`, límites `[call]`). Módulo `flow`
  con el nodo `call`. Ejemplos en `crates/modules/hello/examples/` y
  `crates/modules/flow/examples/call.toml`.
- **Hooks del agente** (`docs/13-blocks.md` §4, protocolo `1.9`): la caja Agente tiene
  la entrada `hooks`. Un nodo `rule` (módulo `hooks`) veta una herramienta o añade
  contexto en un evento (`pre_tool`, `stop`, `session_start`…). Con Claude Code en perfil
  aislado se escriben en su `settings.json` y llaman al puente `regulus hook`; el core los
  reenvía con `hook.dispatch` y espera como mucho `[hooks] timeout_ms`. opencode aún no.
  Ejemplo en `crates/modules/hooks/examples/guarded.toml`.
- **Agentes** (`docs/12-agents.md`): la caja Agente y los motores Claude Code y opencode,
  que hablan con su CLI por ACP. Cada motor detecta las instalaciones del PC
  (`claude.detect`, `opencode.detect`) y usa la que elijas con tu perfil
  (`config_dir`). Ejemplo en `crates/modules/agent/examples/agents.toml`.
- Ejecuciones: `execution.start` / `execution.stop` (protocolo `1.2`) y los comandos
  `regulus run`, `regulus stop`, `regulus ps` y `regulus agent prompt`. Un nodo al que
  le falta una entrada obligatoria queda desactivado sin tumbar el resto.
- El core: `regulus serve` sirve JSON-RPC 2.0 por WebSocket en loopback, protocolo `1.1`
  (`hello`, `profile.list`, `profile.get`, `profile.check`, `module.list`,
  `execution.list`). Pide el token de `<home>/token` (`Authorization: Bearer` o
  `?token=`) y un `hello` compatible antes de cualquier otro método. La UI funciona contra
  él con `VITE_RPC_URL=ws://127.0.0.1:<puerto>/?token=<token>`.
- `regulus serve --without <módulo>` arranca sin ese módulo; un módulo que falla o entra
  en pánico queda `broken` sin tumbar el resto.
- Comandos cliente: `regulus call`, `regulus module ls`, `regulus profile ls` y
  `regulus graph <perfil>`.
- Módulo `hello` (`hello.greet`) y matriz de ablación automática sobre los módulos
  instalados.
- Harness de git: convenciones de commit en `.gitconventions`, hooks en `.githooks/`,
  validador ejecutable a mano (`scripts/commit-lint.sh`), escáner de credenciales
  (`scripts/secrets-scan.sh`) e instalador (`scripts/install-hooks.sh`). Reglas en
  `docs/09-git.md`.
- `.gitignore` y plantilla de mensaje de commit (`.gitmessage`).
- **Jueces y disparadores** (`docs/13-blocks.md` §3.2): módulo `judge` con Juez, Clasificador,
  Extractor y Rúbrica en árbol sobre un proveedor `openai` o `anthropic` (Ollama, LM Studio o
  nube) con schema estricto, reintento y salida `error`; el motor se cambia por cualquier
  `judge.engine@1`. Módulo `triggers` con Cron, Webhook (`regulus call triggers.webhooks`),
  Evento del bus, Chat con transcript (`triggers.transcript`), Inactividad y Umbral. Ejemplos en
  `crates/modules/judge/examples/judge.toml` y `crates/modules/triggers/examples/triggers.toml`.
