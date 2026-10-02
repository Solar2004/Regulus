# 00 — Principios: el harness de Regulus

> **v0.1** · 2026-10-02 · ✅ **DECIDIDO por Alexander**
> **Este documento manda sobre todos los demás.** Si una decisión técnica de cualquier
> otro documento viola un principio de aquí, el principio gana y la decisión se revisa.
>
> Vale para **todo agente** que trabaje en este repo: Claude Code, opencode, Codex,
> Cursor o cualquier otro.

---

## La regla de oro

> **"Nuestro producto tiene que ser como el agua."** — Alexander

El agua toma la forma del recipiente. No pelea con él. No exige que el recipiente cambie.

Aplicado a Regulus: **el software se adapta a lo que el usuario quiere hacer, no al
contrario.** Cuando haya duda entre dos diseños, gana el que deje más opciones abiertas
al usuario — no el que sea más fácil de escribir.

---

## Los cinco principios

Cada uno tiene una **prueba concreta**. Un principio sin prueba es decoración, y la
decoración se incumple sin que nadie se dé cuenta.

### 1. Flexible

**Nada que el usuario pueda querer cambiar está escrito a fuego en el código.**

**Prueba:** busca literales en el código. Si un número, una ruta, un puerto, un color, un
atajo de teclado o un nombre aparece escrito directamente en la lógica, **es un bug**, no
un detalle pendiente. Va a configuración con un valor por defecto.

```rust
// ❌ mal
let port = 7777;
if agents.len() > 10 { warn!("demasiados agentes"); }

// ✅ bien
let port = cfg.server.port;              // default 7777
if agents.len() > cfg.limits.warn_agents { … }
```

### 2. Modular

**Quitar cualquier pieza deja el resto funcionando.**

**Prueba:** la **matriz de ablación** ([`03-modules.md` §4](03-modules.md)). Con N
módulos, Regulus arranca N veces quitando uno cada vez, y el resto responde. Automatizado
en CI. No es una intención: es un test que pasa o falla.

Corolario que se verifica leyendo el código: **si en el core aparece
`if module.id == "algo"`, el principio está roto.**

### 3. Rápido

**Hay presupuestos numéricos, no adjetivos.**

**Prueba:** estos números. Si se superan, es una regresión y se trata como un bug:

| Operación | Presupuesto |
|---|---|
| Respuesta de la UI a una interacción | **< 16 ms** (un frame a 60fps) |
| Arranque del core (`regulus serve` listo) | **< 200 ms** |
| Método RPC de lectura (`module.list`, `agent.list`) | **< 5 ms** |
| Latencia de tecla a píxel en la terminal | **< 30 ms** |
| Arranque de la app hasta primera pantalla útil | **< 1 s** |

🔶 Son propuestos: si al medir resultan irreales, se ajustan **con el número medido
escrito al lado**, no se borran.

### 4. Personalizable

**Todo lo que la interfaz muestra se puede reconfigurar sin recompilar.**

**Prueba:** layout de paneles, atajos de teclado, tema, qué módulos se cargan, qué
columnas se ven. Todo vive en configuración legible por humanos. Si para mover un panel
hay que tocar un `.tsx`, el principio está roto.

### 5. Infraestructura

**Todo lo que la GUI puede hacer, el CLI puede hacer.**

**Prueba:** por cada capacidad nueva, existe el comando equivalente en `regulus-cli`. Si
algo solo se puede hacer desde la ventana, Regulus dejó de ser infraestructura y pasó a
ser una app.

Esto ya está garantizado por arquitectura: los dos límites de
[`02-architecture.md` §1](02-architecture.md) hacen que la GUI sea **un cliente más**. El
principio es la razón de que ese diseño exista, no una consecuencia accidental.

---

## El guardarraíl: flexibilidad ≠ alcance infinito

Hay que decir esto explícitamente, porque "máxima flexibilidad" y el riesgo nº1 del
proyecto ([`05-roadmap.md` §4](05-roadmap.md): *construir el framework y nunca las
features*) empujan en la misma dirección peligrosa.

**La distinción que resuelve el conflicto:**

| Flexibilidad es… | Flexibilidad NO es… |
|---|---|
| **No cerrar puertas.** No hardcodear, no asumir un solo caso, no acoplar. | **Construir todas las habitaciones hoy.** Soportar casos que nadie ha pedido. |
| Que `project_id` exista en el modelo desde el día 1 | Implementar sincronización entre 20 proyectos en v1 |
| Que los colores pasen por variables | Hacer 6 temas |
| Que el trait `Module` admita un `ProcessModule` futuro | Escribir el `ProcessModule` ahora |

**La prueba:** ¿esto me cierra una puerta? Si sí, haz el trabajo ahora — es barato.
¿Esto es una habitación que nadie ha pedido? No la construyas — [`01-vision.md`
§4.1](01-vision.md) manda: toda capacidad del core necesita un caso de uso que la pida.

Ser como el agua significa **no endurecerse**. No significa llenar el vaso entero el
primer día.

---

## Cómo se usa este documento

Al revisar código o diseño, estas cinco preguntas se contestan antes de aprobar:

1. ¿Hay algún literal que debería ser configuración? *(Flexible)*
2. ¿Sigue pasando la matriz de ablación? *(Modular)*
3. ¿Se midió, y entra en presupuesto? *(Rápido)*
4. ¿Se puede cambiar sin recompilar? *(Personalizable)*
5. ¿Existe el comando de CLI equivalente? *(Infraestructura)*

Si alguna respuesta es "no" y no hay una razón escrita, no se aprueba.
