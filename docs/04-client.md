# 04 — El cliente (app y CLI)

> **v1.0** · 2026-10-05 · Lo vigente del cliente, rescatado de la versión anterior
> ([`old/04-client.md`](old/04-client.md), modelo antiguo: 8 métodos, agente en el centro).
> El contrato de métodos actual está en [`composition-preview.md`](composition-preview.md).

---

## 1. El mock es la spec del core

✅ **DECIDIDO** (decisión #9, cliente primero) — La UI se construye contra un mock, y la
lista de métodos del mock **es** la especificación del core.

- **El mock no inventa métodos cómodos para la UI.** Cada método devuelve algo que el
  core puede servir de verdad, con datos que un módulo realmente tiene.
- **Los métodos devuelven estado del sistema**, no objetos preformados para un componente.
  La UI compone.
- **Prueba:** al conectar el core real, la UI no cambia una línea. Si cambia, algún método
  del mock era falso.

---

## 2. Las dos vistas

✅ **DECIDIDO** — Dos proyecciones sobre **un solo modelo de estado**, no dos apps:

| Vista | Para qué |
|---|---|
| **Canvas** | Diseñar: nodos, cables y workflows ([`11-composition.md`](11-composition.md)) |
| **Operar** | Lo que corre ahora: ejecuciones, terminales, logs, errores |

El canvas puede marcar qué nodos están vivos, pero no mezcla estado con diseño.

---

## 3. Estados de primer nivel

✅ **DECIDIDO** — Cada uno se diseña, no es un error genérico:

1. **No instalado** — un módulo ausente se muestra así, **nunca como error**.
2. **Desactivado** — le falta una entrada `required`; se dice cuál.
3. **Roto** — pánico o fallo en `init`; se muestra la razón y se puede recargar.
4. **Sin usar** — `Idle` con `last_used` vacío: instalado y nunca usado, visible.

Iconos y colores de cada estado: [`08-design-system.md`](08-design-system.md).

---

## 4. Conexión con el core

✅ **DECIDIDO** (decisión #8)

- **Transporte:** JSON-RPC 2.0 sobre WebSocket en `127.0.0.1`, token en `~/.regulus/token`.
- **`hello` primero, siempre.** Mismo major de protocolo = compatible; el core soporta N y
  N-1. Con major distinto el cliente no arranca ni se degrada en silencio: dice qué
  versión necesita. Vale también para el mock.
- **Arranque:** la app nunca lanza un segundo core si el puerto responde a `hello`. El core
  auto-lanzado se desprende (cerrar la ventana no lo mata). `autostart = false` lo
  desactiva; sin core, la app muestra el comando exacto a ejecutar.

---

## 5. Multi-proyecto

✅ **DECIDIDO** (decisión #15) — El proyecto activo es **estado del cliente**. Los
métodos de lectura aceptan `project_id` opcional (ausente = todos los proyectos). Así dos
clientes miran proyectos distintos contra el mismo core.

---

## 6. Idioma y tema

✅ **DECIDIDO** (decisión #20)

- **UI en inglés**, con todos los textos visibles en ficheros de traducción desde el día 1
  (`frontend/src/locales/`). Prohibido escribir textos visibles directamente en un `.tsx`.
- **Solo tema oscuro visible**, pero todos los colores pasan por variables CSS de
  [`08-design-system.md`](08-design-system.md). Prohibido hardcodear colores.
