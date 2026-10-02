# 01 — Visión

> **v0.3** · 2026-10-02 · Alexander (rbevans@centaury.ai)
> Por qué existe Regulus y qué significa "módulo" aquí. Si algo en otro documento
> contradice esto, esto gana.

---

## 1. Qué es Regulus

✅ **DECIDIDO** — En palabras de Alexander:

> "Regulus será la infraestructura donde trabajará mi código, la cual se adapta a lo
> que sea. Un hub donde yo pongo funcionalidades, las conecto, por módulos. Quiero un
> día despertarme y querer poner hcom como módulo. Que les quiero dar un rol, que los
> agentes existan en varias familias, que cada uno de manera aislada tenga su propio
> MCP: pues otro módulo que lo conecte y listo. Memoria compartida por proyecto o
> global: pum, lo pongo y listo. Yo tendría que crear los módulos, pero el punto es
> **poder**. Y que en vez de ir a la terminal a ver Claude Code, tenga un IDE visual
> que me permita trabajar."

> "Regulus sería como: puedes buscar módulos y conectarlos en código interno, pero de
> manera que sean módulos — puedas quitar algo y que todo siga funcionando bien. Es
> tipo **Rimuru**: puede consumir habilidades y son suyas."

En una frase:

> **Regulus es un host de módulos con interfaz visual para construir, conectar y operar
> sistemas de agentes de IA. Absorbe código ajeno y lo hace propio, sin perder la
> capacidad de quitarlo.**

El producto no son las features. El producto es **la capacidad de añadir y quitar
features sin que nada se rompa**.

### 1.1 La métrica norte

**Tiempo desde idea hasta módulo funcionando dentro de Regulus.**
Objetivo: un módulo simple, de cero a cargado y visible, en **menos de una hora**.

### 1.2 El invariante norte

**Quitar cualquier módulo deja el resto del sistema funcionando.**

No es un buen deseo. Es una propiedad verificable: ver la matriz de ablación en
[`03-modules.md` §4](03-modules.md).

---

## 2. La doctrina Predator

✅ **DECIDIDO** — La idea central del proyecto. Hay que entenderla antes de tocar
arquitectura. **Regulus no es un sistema de plugins de terceros.**

En *Tensura*, Rimuru usa Predator: analiza algo, lo consume, y la habilidad pasa a ser
**genuinamente suya** — integrada en su cuerpo, no un objeto que lleva encima. Puede
combinarla con otras, y puede dejar de usarla.

Eso es un módulo de Regulus:

| Lo que **NO** es | Lo que **SÍ** es |
|---|---|
| Un binario de otro que Regulus lanza | Código absorbido, reescrito para encajar |
| `hcom --flags` por debajo | `regulus spawn agents` — los comandos de hcom desaparecen |
| Un plugin que respeta una API pública | Código interno, nativo, de primera clase |
| Algo que se instala desde un marketplace | Algo que **tú** escribes o canibalizas |

Aplicado a hcom: se le quita todo su CLI, se toma la idea y el código Rust, y se
implementa dentro del sistema de Regulus con la forma que Regulus necesita.

### 2.1 Absorbido no significa fundido

La parte difícil de la doctrina. Si el código absorbido se mezcla con el core, se pierde
el invariante de §1.2 y Regulus se convierte en un monolito con buenas intenciones.

Un módulo absorbido mantiene **tres propiedades, siempre**:

1. **Vive en su propio crate.** Fronteras físicas, no solo conceptuales.
2. **El core no lo conoce por nombre.** Solo por capacidad.
3. **Regulus arranca sin él** y el resto funciona.

Estas tres no son negociables. Son la diferencia entre Regulus y un monolito.

### 2.2 Convención de atribución

✅ **DECIDIDO** — Cuando un módulo nace de código ajeno, lleva en su honor el nombre del
autor original. No es decoración: es el registro de procedencia.

```toml
[module.origin]
derived_from = "hcom"
author       = "aannoo"
repo         = "https://github.com/aannoo/hcom"
license      = "MIT"
honors       = "aannoo"   # la UI lo muestra en la ficha del módulo
```

Cumple la licencia y mantiene la memoria de dónde salió cada habilidad.

---

## 3. Qué NO es Regulus

✅ **DECIDIDO** — Las exclusiones son tan importantes como la visión.

| No es | Por qué | Usar en su lugar |
|---|---|---|
| **Un editor de código** | Zed ya lo hace mejor. Un motor de texto + LSP + render a 120fps es un proyecto completo por sí solo. | **Zed** |
| **Un clon de Orca** | Orca es un producto terminado con opiniones fijas. Regulus es una base sin opiniones donde las opiniones las pone Alexander. | — |
| **Un wrapper de Claude Code** | No debe acoplarse a un agente concreto. Claude Code es *un* módulo posible, no el cimiento. | — |
| **Un marketplace de plugins** | Los módulos son código propio absorbido (§2), no paquetes de terceros. | — |
| **Un producto para vender (de momento)** | Infraestructura personal primero. Ninguna decisión se toma por "qué querría un usuario hipotético". | — |

### 3.1 El anti-objetivo principal

**No construir features antes de que el sistema de módulos funcione.**

La tentación será "primero hcom integrado directo, luego lo modularizamos". Eso produce
un wrapper de hcom con plugins pegados después. **El primer módulo debe cargarse por el
mismo camino que el módulo número cincuenta.**

---

## 4. Casos de uso canónicos

✅ **DECIDIDO** — Los módulos que Alexander nombró. Son el banco de pruebas del diseño:
si la arquitectura no soporta los cinco con elegancia, la arquitectura está mal.

| # | Módulo | Qué hace | Qué le exige al core |
|---|---|---|---|
| **UC-1** | **Agentes** (de hcom) | Comunicación entre agentes vía terminales | Lanzar y supervisar procesos de larga vida; PTYs; render de terminal |
| **UC-2** | **Roles** | Dar a un agente un rol/persona: su prompt, permisos, encargo | Identidad de agente que otros módulos puedan leer y decorar |
| **UC-3** | **Familias** | Agrupar agentes en familias con config propia | Jerarquía sobre las identidades de UC-2; visualizable |
| **UC-4** | **MCP aislado** | Cada agente con su propio servidor MCP, aislado | Ciclo de vida por agente; config generada; aislamiento real |
| **UC-5** | **Memoria** | Memoria para agentes, por proyecto o global | Store con noción de proyecto; expuesto a los agentes; consultable |

### 4.1 Las cinco capacidades que el core necesita

Leyendo los cinco casos juntos, el core necesita exactamente esto. **Nada más en v1:**

1. **Ciclo de vida de procesos** — arrancar, parar, supervisar, reiniciar, leer salida.
2. **Identidad de agente** — un objeto `Agent` al que distintos módulos cuelgan cosas
   (rol, familia, MCP, memoria) **sin conocerse entre ellos**. La abstracción central.
3. **Noción de proyecto** — alcance para memoria, config y estado.
4. **Estado observable** — la UI y los módulos reaccionan a cambios sin polling.
5. **Mediación entre módulos** — UC-3 necesita identidades de UC-2; UC-5 debe exponerse
   a agentes de UC-1. **Los módulos no pueden ser silos.**

El punto 5 es el que mata los diseños ingenuos; se resuelve desde el principio.

**Regla de alcance:** toda capacidad del core debe estar justificada por un UC de esta
tabla. Si ningún UC la pide, no se construye.

---

## 5. Requisitos que salen del workflow actual

✅ **DECIDIDO** — Medido sobre `~/Projectos` el 2026-10-02. Cada problema observado se
convierte en un requisito del diseño. Los datos están en
[`research.md` §3](research.md).

| # | Problema observado | Requisito para Regulus |
|---|---|---|
| 1 | Se copia el proyecto en vez de ramificar (`aicli-ultimate` / `-hcom`, `-old`, `Kore-old`) | **Probar un módulo no requiere copiar Regulus.** Un crate nuevo en el workspace, no un fork del host. |
| 2 | 150 skills instaladas en un perfil, 17 en otro; se instala más rápido de lo que se consolida | **El registro muestra estado real:** activo / instalado-sin-usar / roto, con fecha de último uso. |
| 3 | 19 carpetas sin git: cosas que nacen sin forma de morir limpias | **Desinstalar = borrar el crate y una línea del registro.** Cero residuos en el core. |
| 4 | 29 `CLAUDE.md`/`AGENTS.md` dispersos; dos perfiles con versiones distintas de la misma skill | **Un único `regulus.toml` por proyecto.** El manifest es la fuente de verdad. |
| 5 | `ccmodel` existe porque el sistema no dice qué modelo corre | **El shell siempre muestra qué está corriendo:** módulos, agentes, procesos, modelo. Información visible, no un comando que preguntas. |

---

## 6. Nombre e identidad

✅ **DECIDIDO** — El nombre es **Regulus**. Material para branding:

- **Regulus** es α Leonis, la estrella más brillante de la constelación de Leo.
- Latín: **diminutivo de *rex*** — "pequeño rey", "principito".
- Nombre tradicional: **Cor Leonis**, "el corazón del león".
- Una de las cuatro **Estrellas Reales** de la astronomía persa antigua (con Aldebarán,
  Antares y Fomalhaut): los cuatro guardianes del cielo.
- Astrofísica: **no es una estrella, es un sistema de cuatro** — Regulus A (rota a
  ~320 km/s, tan rápido que está achatada y al borde de romperse) más tres compañeras
  en órbita.

La metáfora coincide con la arquitectura real: **un núcleo brillante con satélites en
órbita** = el core con sus módulos. Y "corazón del león" dice lo que es: el centro que
bombea, no la piel.

🔶 **PROPUESTO** para el logo:

- Dirección: núcleo + cuerpos en órbita. Geométrico, no ilustrativo. Funciona a 16×16 y
  en monocromo.
- Evitar: el cliché del cerebro con circuitos, degradados morados de IA, y leones
  literales (Regulus es la estrella, no el león).
- Tono: instrumento astronómico, no app de consumo.

❓ **ABIERTO** — Paleta, tipografía, y si hay wordmark además del símbolo. Es BRIEF-1.

---

## 7. Glosario

| Término | Significado en Regulus |
|---|---|
| **Core** | `regulus-core`. Carga módulos, media entre ellos, gestiona procesos y estado. |
| **Cliente** | Cualquier cosa que habla con el core por socket: GUI, CLI, móvil. |
| **Módulo** | Unidad de funcionalidad. Un crate del workspace detrás del trait `Module`. Código absorbido, no plugin de terceros (§2). |
| **Capacidad** | Contrato con nombre que un módulo provee o consume (`memory.store`). Los módulos se encuentran por capacidad, **nunca** por nombre. |
| **Agente** | Un agente de IA en ejecución. La abstracción central. |
| **Proyecto** | Alcance para memoria, config y estado. Normalmente un repo. |
| **Shell** | La capa de UI. Un cliente del core, no el core. |
| **Doctrina Predator** | Absorber código ajeno y hacerlo propio sin perder la capacidad de quitarlo (§2). |
| **Matriz de ablación** | Arrancar Regulus N veces quitando un módulo cada vez. La prueba de §1.2. |
| **ADE** | "Agent Development Environment", término de Orca. Regulus **no** se describe así. |

---

## Historial

| Fecha | Versión | Cambio |
|---|---|---|
| 2026-10-02 | v0.1 | Visión y no-objetivos iniciales. |
| 2026-10-02 | v0.2 | Doctrina Predator, atribución, invariante de ablación. |
| 2026-10-02 | v0.3 | Documento dividido. Este fichero conserva el "por qué"; arquitectura, contrato de módulos, cliente y roadmap pasan a `02`–`05`. |
