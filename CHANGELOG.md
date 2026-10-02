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

- Harness de git: convenciones de commit en `.gitconventions`, hooks en `.githooks/`,
  validador ejecutable a mano (`scripts/commit-lint.sh`), escáner de credenciales
  (`scripts/secrets-scan.sh`) e instalador (`scripts/install-hooks.sh`). Reglas en
  `docs/09-git.md`.
- `.gitignore` y plantilla de mensaje de commit (`.gitmessage`).
