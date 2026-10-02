# 07 — Ecosistema: qué ya existe y no hay que escribir

> **v0.4** · 2026-10-02
> Herramientas, protocolos y harnesses que sirven a Regulus. Investigado el 2026-10-02.
> **Lee esto antes de escribir cualquier integración con agentes.**

---

## 1. El hallazgo principal: ACP (Agent Client Protocol)

**Existe un protocolo estándar para que un cliente hable con agentes de IA, y ~45
agentes ya lo implementan.** Regulus no tiene que escribir integraciones una por una.

| Dato | Valor |
|---|---|
| Web | https://agentclientprotocol.com |
| Crate Rust | **`agent-client-protocol` 2.2.0** · 4.8M descargas · upd 2026-09-18 |
| Versión del protocolo | v2 (v1 todavía documentada, con guía de migración) |
| Origen | Zed Industries |
| Transportes | JSON-RPC por stdio (agentes locales) · HTTP/WebSocket (remotos) |
| Otras libs | TypeScript, Python, Kotlin, Java |

### 1.1 Agentes que ya hablan ACP

Los relevantes para Alexander, de una lista de ~45:

**Claude Agent** (vía el adaptador SDK de Zed) · **Codex CLI** (vía adaptador oficial) ·
**Gemini CLI** · **OpenCode** · **Cursor** · **GitHub Copilot** (preview pública) ·
**Kimi CLI** · **Pi** · **Qwen Code** · **Cline** · **Goose** · **OpenHands** ·
**Mistral Vibe** · **Factory Droid** · **Junie (JetBrains)** · **Kiro CLI** ·
**Docker cagent** · **Augment Code** · **Blackbox AI** · **Stakpak**

Coincide casi exactamente con la lista que hcom dice soportar en su descripción. hcom
escribió esas integraciones a mano; ACP las estandariza.

### 1.2 Qué cubre el protocolo

Leído del índice de v2:

| Área | Métodos |
|---|---|
| Sesiones | crear, reanudar, listar, borrar, *fork* (en RFD) |
| Conversación | ciclo de vida del prompt, bloques de contenido, cancelación |
| Herramientas | reporte de tool calls con su estado |
| **Terminales** | **ejecutar y gestionar comandos de terminal** |
| Sistema de ficheros | acceso al FS del cliente |
| Permisos | *elicitation* — pedir información estructurada al usuario |
| Planes | el agente comunica su plan de ejecución |
| Modos y config | modos de sesión, selectores de configuración |
| Comandos | slash commands que el agente anuncia al cliente |
| Extensibilidad | datos y capacidades propias |

### 1.3 Qué significa para Regulus

**Regulus es, técnicamente, un ACP Client.** Igual que Zed, Obsidian Harness o Martty.

Lo que se obtiene gratis:
1. 45 agentes sin escribir una integración por agente
2. Terminales, permisos y tool calls ya modelados
3. La UI puede mostrar planes y tool calls de forma uniforme para cualquier agente
4. Un **ACP Registry** oficial para descubrir e instalar agentes

### 1.4 ACP vs hcom: no es una elección

> ⚠️ Una versión anterior de este documento decía "ACP no cubre el eje agente↔agente".
> Es cierto como feature del protocolo, pero lleva a la conclusión equivocada. Corregido
> aquí.

**Lo que hace que hcom funcione es que inyecta mensajes como si los escribiera el
usuario.** Verificado en el código: `src/delivery.rs:458` define `inject_text(port,
text)` y `:475` define `inject_enter(port)`. hcom teclea en el PTY del agente y pulsa
Enter. Desde dentro, el agente cree que le habló su usuario — y por eso **actúa**.

**ACP hace exactamente lo mismo, por un canal mejor.** `session/prompt` es el canal del
usuario: un cliente ACP *es* el usuario desde el punto de vista del agente. Mismo efecto,
sin teclear bytes en una TUI y esperar que los parsee bien.

Y el eje horizontal se construye **sobre** ACP, mediado por Regulus:

```
  agente A invoca la herramienta "mándale esto a B"
        │
        ▼
     Regulus  ──── media ────┐
        │                    │
        ▼                    ▼
  session/prompt → B    registra en el store
        │
        ▼
  B lo recibe como mensaje de usuario  →  actúa
```

No se pierde nada de lo que hcom aporta. La horizontalidad la da **Regulus mediando entre
dos sesiones ACP**, no el protocolo.

### 1.4.1 El número que justifica ACP

Peso real de `src/hooks/` en hcom — pegamento por agente:

| Fichero | Tamaño |
|---|---|
| `claude.rs` | 121 KB |
| `codex.rs` | 98 KB |
| `gemini.rs` | 96 KB |
| `common.rs` | 62 KB |
| `opencode.rs` | 43 KB |
| `claude_args.rs` | 41 KB |
| resto (`kilo`, `cline`, `antigravity`, `family`, …) | ~88 KB |
| **total** | **~549 KB** |

Es el **~10% de hcom**, dedicado a integraciones que se rompen cada vez que un CLI cambia
su TUI o sus flags.

**ACP no ahorra la idea. Ahorra esos 549 KB y su mantenimiento perpetuo.**

### 1.4.2 Lo que hcom tiene y ACP no

Argumentos reales para conservar también su mecanismo:

1. **hcom se engancha a una sesión que el humano ya tiene abierta en su terminal.** ACP
   crea la sesión él mismo (`session/new`): esa sesión pertenece a Regulus, no a la
   terminal del usuario.
2. **hcom funciona con cualquier CLI que tenga terminal**, hable ACP o no.
3. **Riesgo concreto:** el Claude Code de Alexander lleva su cadena de proxies, sus ~150
   skills y sus `CLAUDE.md`. La ruta ACP para Claude es el adaptador del Agent SDK de Zed
   — **no es su binario `claude`**. Puede no arrastrar nada de ese setup.
   **Hay que medirlo, no suponerlo:** es el objetivo de BRIEF-6.

### 1.4.3 Diseño resultante

🔶 **PROPUESTO** — **Los dos, con ACP primero:**

| Transporte | Cuándo | Qué da |
|---|---|---|
| **ACP** | Agentes que lo hablan (~45) | Sesiones, tool calls, terminales, permisos y planes como datos estructurados. Cero hooks por agente. |
| **Inyección en PTY** (el `inject_text` de hcom, absorbido) | Agentes sin ACP · sesiones que el humano ya tiene abiertas | Universalidad. Funciona con cualquier cosa que tenga terminal. |

Lo que se absorbe de hcom bajo la doctrina Predator:

- ✅ La **idea**: agentes que se hablan entre sí como usuarios
- ✅ El modelo de datos: mensajes, suscripciones, enrutado
- ✅ Familias
- ✅ El inyector de PTY, como fallback
- ❌ **No** los ~549 KB de hooks por agente

### 1.5 Dos RFDs que rozan ideas de Alexander

En https://agentclientprotocol.com/rfds/ hay propuestas abiertas:

| RFD | Qué propone | Relación |
|---|---|---|
| **Agent Extensions via ACP Proxies** | Encadenar proxies que envuelven a un agente para extenderlo | Es literalmente la lectura A de [`ideas/boxes.md`](ideas/boxes.md) (caja = envoltorio), ya pensada por otros |
| **MCP-over-ACP** | Peticiones MCP sin estado a través de ACP | Afecta a UC-4 (MCP aislado por agente) |
| **Forking of existing sessions** | Bifurcar una sesión existente | Útil para UC-3 (familias) |

**Leer el RFD de proxies antes de diseñar las cajas.** No para copiarlo, para no repetir
su trabajo de análisis.

❓ **ABIERTO — decisión #12** — ¿Regulus habla ACP para integrar agentes?
🔶 Recomendación: **sí, y es lo más importante que ha salido de la investigación.** Ver
[`05-roadmap.md` §3](05-roadmap.md).

---

## 2. MCP: `rmcp`

| Dato | Valor |
|---|---|
| Crate | **`rmcp` 3.5.0** · 31.4M descargas · upd 2026-09-28 |
| Qué es | SDK oficial de Model Context Protocol para Rust |

Sirve a dos casos de uso directamente:

- **UC-4 (MCP aislado)** — Regulus levanta un servidor MCP por agente. `rmcp` da el lado
  servidor.
- **UC-5 (memoria)** — la forma natural de exponer la memoria compartida a los agentes es
  **como un servidor MCP**. Así cualquier agente la consume sin integración especial.

Es decir: UC-5 no necesita que cada agente sepa de Regulus. Habla MCP y ya.

---

## 3. Registro de módulos: candidatos para la decisión #10

Para el mecanismo de registro del trait `Module`
([`03-modules.md` §2](03-modules.md)):

| Crate | Versión / descargas | Qué hace | Valoración inicial |
|---|---|---|---|
| **registro a mano** | — | Un `Vec<Box<dyn Module>>` construido explícitamente en el arranque | **Candidato más fuerte.** Explícito, fácil de depurar, y la ablación es trivial (`--without` filtra la lista) |
| `inventory` | `0.3.24` · 135M dl | Registro distribuido por tipos; los módulos se auto-registran | Mágico. Dificulta la ablación porque el registro ocurre antes de `main` |
| `linkme` | `0.3.37` · 33.9M dl | Slices distribuidos en tiempo de enlazado | Mismo problema que `inventory` |
| `abi_stable` | — | ABI estable para cargar `.so` en caliente | Resuelve un problema que Regulus no tiene (módulos in-process, mismo compilador) |
| `extism` | `1.30.0` · 728k dl | Runtime de plugins WASM | Descartado: WASM no da PTYs ni sockets ([`02-architecture.md` §2](02-architecture.md)) |

**Observación para BRIEF-3:** la auto-registración de `inventory`/`linkme` pelea
directamente con la matriz de ablación ([`03-modules.md` §4](03-modules.md)), porque un
módulo que se registra solo es un módulo que no puedes quitar con un flag. El registro
explícito parece ganar por eso, no por simplicidad.

---

## 4. Implementaciones de referencia que conviene leer

No para copiar: para ver qué problemas aparecen.

| Proyecto | Por qué leerlo |
|---|---|
| **Anycode** (`anycode-ade/anycode`) | IDE web con **backend Rust y frontend React**. El stack más cercano al de Regulus. |
| **Martty** (`openma-ai/Martty`) | Cliente ACP en **Rust + ratatui**. Cómo se consume el crate de ACP en Rust. |
| **Obsidian Harness** (`vlln/obsidian-harness`) | "Obsidian como cockpit para agentes ACP", cada sesión un fichero. Modelo de estado interesante. |
| **Zed** (`zed-industries/zed`) | Autor de ACP y de `gpui`. Referencia de sistema de extensiones. |
| **Orca** (`stablyai/orca`) | Referencia de UX, no de arquitectura ([`research.md` §1](research.md)). |

---

## 5. Lo que NO se va a usar

Dejado por escrito para que nadie lo reabra sin motivo:

| Herramienta | Por qué no |
|---|---|
| **Electron** | Contradice el requisito de Rust; el core es Rust igual |
| **gpui** (de Zed) | No es lib estable para terceros; docs casi inexistentes; API cambia sin aviso |
| **WASM / `extism`** | Los casos de uso necesitan PTYs, procesos hijos y sockets |
| **React Flow** | La topología es de solo lectura; `dagre` + SVG bastan ([`06-stack.md` §2.3](06-stack.md)) |
| **`node-pty`** | Es de Node. `portable-pty` hace lo mismo en Rust y deja el PTY del lado del core |
| **headroom / token-proxy** | Son de la cadena personal de Alexander, no de Regulus. Aclarado explícitamente. |

---

## 6. Resumen: qué se ahorra Regulus

| Trabajo que NO hay que hacer | Gracias a |
|---|---|
| Integrar 45 agentes uno a uno | `agent-client-protocol` |
| Escribir un emulador de terminal | `xterm.js` + `portable-pty` |
| Escribir un servidor MCP | `rmcp` |
| Diseñar un protocolo cliente↔core desde cero | JSON-RPC 2.0 + `jsonrpsee` |
| Montar empaquetado multiplataforma | `cargo-dist` + `tauri build` |

Lo que **sí** es trabajo propio de Regulus, y por tanto donde está su valor:

1. El sistema de módulos y su invariante de ablación
2. El eje horizontal agente↔agente (hcom absorbido) — §1.4
3. El sistema de cajas, si se define ([`ideas/boxes.md`](ideas/boxes.md))
4. Las dos vistas y el modelo de estado que las alimenta
