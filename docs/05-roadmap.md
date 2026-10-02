# 05 — Roadmap, decisiones y briefs

> **v0.3** · 2026-10-02
> Qué toca ahora, qué está decidido, qué está abierto, y qué puede hacer cada agente.

---

## 1. Alcance de v1

✅ **DECIDIDO** — *"No haremos nada de hcom ahora. Lo que haremos es entender y crear la
infraestructura de Regulus, prepararlo para los módulos que tendrá y los futuros, y que
sea super flexible. Hacer de momento la base: una app sin funcionalidades, algunas
opciones, versión CLI."*

v1 entrega la base, sin features. Y con el cliente primero
([`04-client.md` §1](04-client.md)):

| Fase | Qué | Entregable |
|---|---|---|
| **A** | Mock server con los 8 métodos de [`04-client.md` §3](04-client.md) | JSON enlatado servido por socket |
| **B** | `ui/` vista OPERAR contra el mock, solo lectura | Pantalla que muestra estado |
| **C** | `ui/` vista TOPOLOGÍA + los 4 estados vacíos | Ambas vistas funcionando |
| **D** | `regulus-proto` extraído del mock | Tipos compartidos |
| **E** | `regulus-core` + `regulus-cli` implementan los 8 métodos | `regulus serve` real |
| **F** | Módulo `hello` + matriz de ablación | La modularidad, verificada |

Las abstracciones del core (`Agent`, `Isolation`, `Project`) se **definen** en la fase E
aunque nadie implemente features encima
([`02-architecture.md` §4](02-architecture.md)).

### 1.1 Definición de terminado para v1

- [ ] `regulus serve` arranca y expone el socket
- [ ] La UI funciona contra el core real **sin cambiar una línea** respecto al mock
- [ ] `regulus module ls` lista `hello` con su manifest y su estado
- [ ] `hello` provee una capacidad y el CLI la invoca a través del core
- [ ] `regulus serve --without hello` arranca limpio
- [ ] **Añadir un segundo módulo trivial no requiere tocar `regulus-core`**

Los dos últimos son los que importan. El penúltimo prueba la modularidad; el último
prueba que el contrato no se filtró.

### 1.2 Estimaciones

| Trabajo | Tiempo |
|---|---|
| Fase A (mock server) | **medio día** |
| Fases B + C (ambas vistas, solo lectura) | **1-2 semanas** (doble coste aceptado) |
| Fases D + E (proto + core + CLI) | **2-3 días** |
| Fase F (módulo `hello` + matriz de ablación) | **medio día** |

---

## 2. Decisiones cerradas

| # | Decisión | Resultado | Dónde |
|---|---|---|---|
| **1** | Forma del repo | Workspace Rust, core sin UI, GUI como cliente | [`02` §3](02-architecture.md) |
| **2** | Qué es un módulo | **Crate Rust in-process** tras el trait `Module` | [`02` §2](02-architecture.md), [`03` §2](03-modules.md) |
| **3** | Alcance de red | **Core como servidor desde el día 1** (socket local) | [`02` §1](02-architecture.md) |
| **4** | Shell visual | **Dos vistas** (operar + topología) sobre un modelo | [`04` §2](04-client.md) |
| **6** | Alcance de v1 | Solo la base. Nada de hcom. | §1 |
| **7** | Framework de GUI | **Tauri 2** + React 19 + Vite + Tailwind 4 + zustand. Decisivo: la terminal. | [`06` §1](06-stack.md) |
| **8** | Transporte | **JSON-RPC 2.0 sobre WebSocket** en `127.0.0.1`, token en `~/.regulus/token` | [`06` §4](06-stack.md) |
| **9** | Orden de construcción | **Cliente primero**, contra mock server | [`04` §1](04-client.md) |
| **13** | Versionado | **Tres ejes**; el del protocolo es el único estricto, con handshake obligatorio | [`06` §5](06-stack.md) |
| **14** | Terminal | xterm.js 6 en el webview · `portable-pty` **en el core** | [`06` §3](06-stack.md) |
| **0** | **Los 5 principios** | **Flexible · Modular · Rápido · Personalizable · Infraestructura.** Mandan sobre todo lo demás. | [`00-principles.md`](00-principles.md) |
| **15** | Multi-proyecto | **Varios simultáneos.** `project_id` en todo el modelo desde el día 1. | [`04` §8](04-client.md) |
| **16** | Arranque del core | **Los tres modos funcionan.** Auto-lanzar por defecto, nunca obligatorio. | [`04` §9](04-client.md) |
| **17** | Idioma de la UI | **Inglés, con i18n desde el día 1.** Los docs siguen en español. | [`04` §10](04-client.md) |
| **18** | Tema | **Solo oscuro visible, con todos los colores en variables** desde el principio. | [`08-design-system.md`](08-design-system.md) |
| **19** | Harness de git | **Conventional Commits aplicado por hooks**, política en `.gitconventions` (no en los hooks). Master avisa, no bloquea; reescribir historia publicada sí bloquea. | [`09-git.md`](09-git.md) |
| **20** | Idioma del código y los commits | **Inglés** (orden de Alexander). Extiende la #17: código, UI y commits en inglés; docs en español, solo eso. | [`09-git.md`](09-git.md) |

---

## 3. Decisiones abiertas

| # | Decisión | Opciones | Recomendación | Bloquea |
|---|---|---|---|---|
| **12** | **Transporte hacia los agentes** | Solo ACP / solo inyección en PTY (hcom) / **los dos** | **Los dos, con ACP primero.** No es una elección: `session/prompt` de ACP y el `inject_text` de hcom logran lo mismo — que el agente reciba un mensaje *como si fuera del usuario*. ACP ahorra los ~549 KB de hooks por agente; el inyector de PTY da universalidad y sirve para sesiones que el humano ya tiene abiertas. [`07` §1.4](07-ecosystem.md) | Nada de v1, **todo** de v2 |
| **5** | Cuándo se absorbe hcom | Después de v1 / junto al segundo módulo | **Después de v1**, y solo cuando la matriz de ablación exista | Nada de v1 |
| **10** | Registro de módulos | a mano / `inventory` / `linkme` / `abi_stable` | **A mano.** La auto-registración de `inventory`/`linkme` pelea con la matriz de ablación ([`07` §3](07-ecosystem.md)) | Fase E |
| **11** | Sistema de cajas | Ver [`ideas/boxes.md`](ideas/boxes.md) | Sin recomendación: la idea no está definida. Leer antes el RFD de ACP Proxies ([`07` §1.5](07-ecosystem.md)) | `Isolation` en fase E |

**Ninguna bloquea las fases A ni B.** Se puede empezar a construir el cliente ya.

La #11 es la que puede cambiar más cosas: si las cajas se adoptan, `Isolation` deja de
ser un struct fijo y pasa a ser un caso particular de caja. **No implementar `Isolation`
hasta resolverla.**

La #12 no afecta a v1, pero afecta a **todo** lo que venga después, y cambia el valor de
absorber hcom: ACP cubre el eje tú↔agente, hcom cubre el eje agente↔agente. Con ACP
resuelto, hcom se vuelve **más** valioso, no menos, porque es la parte que nadie más
estandariza ([`07` §1.4](07-ecosystem.md)).

---

## 4. Riesgos

| Riesgo | Severidad | Mitigación |
|---|---|---|
| **Construir el framework y nunca las features** | Alta | Toda capacidad del core debe estar justificada por un UC de [`01` §4](01-vision.md). Si ningún UC la pide, no se construye. |
| **El contrato se filtra** — los módulos dependen de internals del core | Alta | §1.1 último punto: añadir el segundo módulo no puede tocar `regulus-core`. |
| **Absorber mata la modularidad** — el código de hcom se funde con el core | Alta | [`01` §2.1](01-vision.md) + la matriz de ablación, construida **antes** de absorber hcom. |
| **La UI bonita y el core que nunca llega** — riesgo propio de empezar por el cliente | Alta | El mock server es la spec del core (fase D extrae `regulus-proto` del mock). La fase E no empieza de cero. |
| **Alcance infinito** — "se adapta a lo que sea" no tiene límite natural | Alta | [`01` §3](01-vision.md) y §1.1. v1 cierra con `hello`, no con hcom. |
| **Un `panic` en un módulo mata el host** (coste aceptado) | Media | `catch_unwind` en el borde de `Module::call` ([`03` §2.2](03-modules.md)). |
| **WebKitGTK en Linux** si se elige Tauri | Media | Regulus no necesita navegador embebido. Si aparece, proceso aparte. Y la decisión es reversible: la GUI es un cliente. |

---

## 5. Briefs para agentes en paralelo

### ✅ BRIEF-1 — Identidad visual y logo — **ENTREGADO** (2026-10-02)
Resultado en `assets/branding/`: monograma R + estrella de cuatro puntas, logo
horizontal y icono de app en SVG y PNG, `.ico` para Windows, y `build_assets.py` para
regenerar. Paleta: azul noche `#0A1424`, azul acero `#587BA7`, blanco frío `#F3F8FF`.
Fichero maestro: `assets/branding/regulus-logo.svg`. Documentado en el `README.md`.
**Pendiente:** confirmar que el icono aguanta a 16px, y la tipografía del wordmark.

### BRIEF-2 — Diseño final de las dos vistas
Parte de los mockups ASCII de [`04-client.md` §2](04-client.md) — son la referencia
aprobada, no un borrador a reinterpretar. Entrega: diseño de alta fidelidad de ambas
vistas, la transición entre ellas, los tres estados de arista de §2.2, y los cuatro
estados vacíos de §4. Incluye la barra permanente de estado.
**Depende de:** decisión #7. **No implementes UI.**

### BRIEF-3 — Registro de módulos in-process en Rust
El trait de [`03-modules.md` §2](03-modules.md) necesita un mecanismo de registro.
Compara: registro explícito a mano, `inventory`, `linkme`, `abi_stable`. Criterios:
ergonomía al añadir un módulo, si permite la ablación
([`03` §4](03-modules.md)), y compatibilidad con `catch_unwind`. Mira cómo lo resuelven
**Zed** (extensiones), **Bevy** (plugins) y **Tower** (layers). Entrega: recomendación
con código de ejemplo mínimo.
**Depende de:** nada. Resuelve la decisión #10.

### BRIEF-4 — Mapa de superficie de hcom
Lee `~/Projectos/hcom` (fork propio de Alexander). Entrega: qué hace su CLI, su esquema
SQLite, cómo gestiona PTYs, y **qué partes del código son transplantables** al módulo
`agents` según la doctrina Predator. Sé concreto: ficheros y funciones.
**Depende de:** nada. **No cambies hcom** (decisión #5).

### BRIEF-5 — Definir el sistema de cajas
Lee [`ideas/boxes.md`](ideas/boxes.md) **y después** el RFD "Agent Extensions via ACP
Proxies" en https://agentclientprotocol.com/rfds/proxy-chains.md — plantea casi lo mismo
que la lectura A de las cajas, y conviene no repetir su análisis.
**Es una entrevista, no un diseño.** La idea es de Alexander y está sin definir; el
trabajo es hacerle las 5 preguntas de §5 de ese documento y escribir lo que responda, no
proponer una arquitectura.
**Depende de:** Alexander. Resuelve la decisión #11.

### BRIEF-6 — Spike de ACP
Lee [`07-ecosystem.md` §1](07-ecosystem.md). Objetivo: **una respuesta, no código que se
conserve.** Conectar `agent-client-protocol` 2.2.0 a **un** agente real (el más fácil de
los que Alexander ya tiene: Claude Agent, Codex CLI, Gemini CLI u OpenCode) y comprobar
qué llega de verdad: sesiones, tool calls, terminales, permisos.
Entrega: qué funciona, qué no, y cuánto trabajo real ahorra frente a escribir la
integración a mano. Etiqueta el código como desechable.
**Depende de:** nada. Resuelve la decisión #12. Estimación: **medio día.**

### Bloqueado — no empezar
- Implementar UI → espera decisión #7 y BRIEF-2
- Implementar `Isolation` → espera decisión #11
- Absorber hcom → espera v1 completo y la matriz de ablación (decisión #5)

---

## 6. Historial

| Fecha | Versión | Cambio |
|---|---|---|
| 2026-10-02 | v0.1 | `VISION.md` único. Visión, no-objetivos, investigación, 6 decisiones abiertas. |
| 2026-10-02 | v0.2 | Doctrina Predator, atribución, matriz de ablación, trait `Module`, mockups de ambas vistas, abstracciones del core. Cerradas #1-#4 y #6. |
| 2026-10-02 | v0.3 | **Documentación dividida** en `01`–`05` + `research.md` + `ideas/`. Cerrada #9 (cliente primero). Añadido el protocolo derivado de las vistas ([`04` §3](04-client.md)). Abiertas #10 (registro) y #11 (cajas). |
