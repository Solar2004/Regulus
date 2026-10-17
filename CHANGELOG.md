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
