# 08 — Sistema visual de Regulus

> Investigación: 2026-10-02. Kit recomendado; todavía no instalado.
> Paleta y muestra visual preparadas por petición de Alexander.

## Kit recomendado

**shadcn/ui + Base UI**, con el tema propio **Regulus Midnight**. El kit aporta los
componentes; esta guía define su apariencia. El código de los componentes queda en
el proyecto y se puede adaptar. Esto encaja con una herramienta de escritorio con
paneles densos, menús contextuales y navegación por teclado.

La documentación oficial confirma el soporte del stack del cliente y la instalación
con Vite. Base UI es la opción por defecto actual de shadcn; Radix sigue soportado.
La elección de Base UI para este proyecto nuevo evita partir de una configuración
antigua. [Introducción](https://ui.shadcn.com/docs),
[compatibilidad](https://ui.shadcn.com/docs/tailwind-v4),
[guía Vite](https://ui.shadcn.com/docs/installation/vite),
[Base UI por defecto](https://ui.shadcn.com/docs/changelog/2026-07-base-ui-default).

🔶 **PROPUESTO** — Adoptar este kit cuando se implemente el cliente. No se han añadido
dependencias ni creado el shell. La decisión de multi-proyecto sigue en su documento
correspondiente; esta paleta no depende de cómo se resuelva.

### Comparación

| Opción | Lo comprobado | Valoración para Regulus |
|---|---|---|
| **shadcn/ui + Base UI** | Código modificable, variables semánticas, soporte del stack. | Mejor ajuste: permite controlar detalles del tema y de cada componente. Hay que mantener el código incorporado. |
| **HeroUI** | React Aria, Tailwind y componentes compuestos personalizables. | Buena alternativa si se prefiere actualizar un paquete central. shadcn nos da control más directo del código visual. |
| **Radix Themes** | Componentes preestilizados y configuración mediante tokens y propiedades. | Útil para empezar rápido, pero sus propios documentos advierten que las personalizaciones profundas y Tailwind pueden encajar peor. |

Fuentes: [HeroUI: introducción](https://heroui.com/en/docs/react/getting-started),
[HeroUI: requisitos](https://heroui.com/en/docs/react/getting-started/quick-start),
[Radix Themes: estilos](https://www.radix-ui.com/themes/docs/overview/styling).
La preferencia es una valoración de diseño para Regulus, no una clasificación universal.

Iconos de interfaz: **Lucide**, trazo de 1.5–1.75 px y tamaño 16–18 px, con etiquetas
en las acciones importantes. La estrella se reserva para la marca.
[Documentación de Lucide](https://lucide.dev/guide/).
shadcn/ui publica su código bajo MIT; al incorporar componentes se conservarán sus
avisos de licencia. [Licencia](https://github.com/shadcn-ui/ui/blob/main/LICENSE.md).

## Lenguaje visual

**Regulus Midnight:** superficies grafito del tema oscuro de Centaury, con reflejos de
acero en el borde superior y azul frío en las acciones seleccionadas. El icono es la
referencia de material, no una textura que haya que repetir por toda la pantalla.

- Superficies opacas por niveles: fondo → panel → menú flotante.
- Un borde superior iluminado, muy tenue, da continuidad con el icono.
- Esquinas suaves: controles compactos y paneles algo más redondeados.
- Azul hielo para acciones y foco; verde, ámbar y rojo solo para estados.
- Evitar fondos de galaxias, partículas, neón, brillos animados y desenfoque detrás
  de cada panel. La información operativa tiene prioridad.
- Estados siempre con texto y, cuando ayude, símbolo; nunca solo con color.

### Referencia de color: Centaury

Actualizado por petición de Alexander: tomar los colores de su web
[Centaury](https://centaury.ai) y [login](https://centaury.ai/login).
La página pública de acceso se observó en modo claro. Para responder a la petición
de tarjetas más oscuras, se usan las variables **`.dark` de su CSS**, no las tarjetas
blancas que muestra esa ruta por defecto.

Correspondencia con el CSS inspeccionado:

| Token de Regulus | Variable de Centaury |
|---|---|
| `canvas` | `.dark --background` |
| `surface`, `sidebar` | `.dark --card`, `--sidebar` |
| `raised` | `.dark --secondary` |
| `hover`, `selected` | `.dark --navbar-accent` |
| `border` | `.dark --border` |
| `text` | `.dark --foreground` |
| `text-secondary` | `.dark --chart-1` (gris claro) |
| `text-muted` | `.dark --navbar-muted-foreground` |
| `control-border` | `.dark --sidebar-ring` |

Los dos últimos son adaptaciones de uso para mantener legibles los textos auxiliares
y los contornos de campos. Se conserva el azul Regulus para `accent`, `focus` y los
estados; no se adopta el verde de `--primary` de Centaury. El logo y el icono conservan
sus colores. La referencia CSS y los valores efectivos están en
[`palette.json`](../assets/design/palette.json), con fecha y alcance de la extracción.

### Fuente de verdad de la paleta

**[`assets/design/palette.json`](../assets/design/palette.json)** contiene los colores,
fuentes, radios, espaciados, tiempos y sombras. No copiar valores hex a componentes.
[`tokens.css`](../assets/design/tokens.css) se genera desde ese archivo.

| Función | Token |
|---|---|
| Fondo global / lateral | `canvas` / `sidebar` |
| Paneles / popovers | `surface` / `raised` |
| Hover / selección | `hover` / `selected` |
| Separadores decorativos | `border` |
| Contornos que identifican campos y botones | `control-border` |
| Acero decorativo / acento interactivo | `steel` / `accent` |
| Foco de teclado | `focus` |
| Texto principal / secundario / auxiliar | `text` / `text-secondary` / `text-muted` |
| Texto sobre botón claro | `on-accent` |
| Estados | `success`, `warning`, `danger`, `info` y sus fondos `*-bg` |

`steel` es decorativo: no usarlo como texto pequeño. `border` separa paneles pero
no basta como único contorno identificador de un campo. Los componentes de shadcn
deben usar `--input` en ese caso. La variable shadcn `--accent` significa superficie
seleccionada; el azul hielo de marca corresponde a `--primary`.

## Tipografía, densidad y movimiento

- **Títulos:** Manrope 600, 20–28 px, espaciado ligeramente cerrado.
- **Interfaz:** fuente del sistema, 13–14 px, altura de línea 1.5; botones 500.
- **Datos, puertos y logs:** JetBrains Mono, 12–13 px; alternativa monoespaciada local.
- Las fuentes nombradas son una dirección tipográfica, **no archivos incluidos**.
  La muestra usa alternativas locales; al incorporarlas a la app se empaquetarán
  localmente con sus licencias, sin depender de Google Fonts en ejecución.
- Filas 36–40 px; controles 36–40 px. En interfaces táctiles, objetivos de 44 px.
- Espaciado sobre una escala de 4 px. Paneles 16–24 px de padding.
- Radios: 6 px en etiquetas, 10 px en controles, 16 px en paneles, 22 px en diálogos.
- Transiciones de 120–180 ms para fondo, borde y opacidad. No animar cambios de layout
  en logs ni estados críticos. Respetar `prefers-reduced-motion`.

## Componentes que conviene incorporar

Empezar solo por `Button`, `Input`, `Badge`, `Separator`, `Tooltip`, `Tabs`,
`DropdownMenu`, `ContextMenu` y `Dialog`. Añadir otros al necesitarlos. El layout de
paneles y los estados de módulo serán propios; evitar copiar un dashboard completo.

Los gráficos de topología conservan lo decidido en [`06-stack.md`](06-stack.md):
no añadir otra librería de grafo por el kit. Tampoco adelantar paneles de agentes ni
funciones que estén fuera de la primera versión.

## Integración futura

La guía oficial de Vite enlazada arriba es la referencia de instalación. Inicializar
el kit dentro de `ui/` cuando toque implementar el cliente; seleccionar Base UI,
componentes sin React Server Components y variables CSS. Registrar las versiones
resueltas exclusivamente en [`06-stack.md`](06-stack.md) y el lockfile.

En la hoja principal de estilos, **antes de cualquier regla CSS normal**:

```css
@import "tailwindcss";
/* Mantener aquí los imports adicionales que genere el kit. */
@import "../../assets/design/tokens.css";
@import "../../assets/design/shadcn-theme.css";
```

Las rutas del ejemplo asumen `ui/src/index.css`. Sustituir los bloques `:root`,
`.dark` y mapeos de tema iniciales del kit por el adaptador; no conservar un tema
predeterminado posterior que los sobrescriba. El adaptador contiene `@theme inline`
y necesita el procesador Tailwind. `tokens.css` también funciona sin Tailwind.
Marcar el documento con `class="dark"` y conservar la variante dark generada por el kit.
[Variables semánticas de shadcn](https://ui.shadcn.com/docs/theming).

El tema preparado es **solo oscuro**. No supone que ya exista un modo claro.
Revisar los estilos de hover y destructive de los componentes incorporados: no
añadir opacidades que cambien los pares de contraste sin comprobarlos.

## Vista previa y comprobación

Abrir [`preview.html`](../assets/design/preview.html) en un navegador. No necesita
servidor ni red. Enseña la paleta, estados, foco, un filtro y botones. Es un muestrario
HTML, no una implementación ni una prueba de los componentes shadcn.

```sh
python3 assets/design/build_tokens.py
```

El generador verifica 32 pares: texto normal ≥ 4.5:1, foco y contornos de controles
≥ 3:1 sobre las superficies previstas. Esta comprobación de colores no certifica la
accesibilidad de la futura app; habrá que probar teclado, lectores de pantalla,
diálogos y el webview real de Tauri al integrar el kit.
