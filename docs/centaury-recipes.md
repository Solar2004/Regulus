# Centaury → Regulus: recetas visuales verificadas

Fuente: `/home/artorias/Projectos/Centaury/web/`, inspeccionada el 2026-10-02.
El proyecto de Centaury se ha leído, no modificado. Cuando su informe de diseño y
sus componentes difieren, manda el código actual.

## Corrección respecto a la primera paleta

`src/app/globals.css` declara que `.dark` está preparado para el futuro y **no está
activo**. Por tanto, `#181818`, `#10101a` y el verde de ese bloque no describen la
identidad visible de Centaury. La web activa combina blanco con **bandas pizarra**.
Regulus utiliza esas bandas en sus paneles oscuros y conserva el contexto claro
original en las muestras de gráficos. Esta distribución es una adaptación para
Regulus, no un supuesto modo oscuro existente en Centaury.

## Qué se ha trasladado

| Receta | Fuente real | Implementación en Regulus |
|---|---|---|
| Paleta activa y familia navbar/footer | `src/app/globals.css`, `:root` | `palette.json`, `colors` |
| Texto con tinta degradada | `globals.css`, `.text-ink` / `.text-ink-dark` | `.rg-ink` / `.rg-ink-dark` |
| Banda radial | `src/app/client/page.tsx`, tarjeta inferior lateral | `gradient.band-radial`, `.rg-band` |
| Banda lineal | `docs/design-system.md`, receta Principles | `gradient.band-linear`, `.rg-band-linear` |
| Botón oscuro del login | `src/app/login/page.tsx` | `gradient.cta`, `.rg-cta` |
| Contenedor centrado | `src/components/marketing/Container.tsx` | `.rg-container` |
| Shell flotante y cuadrícula | `src/app/login/page.tsx` | `.rg-shell`, `.rg-grid` |
| Borde y luz que siguen el cursor | `src/components/marketing/SpotlightCard.tsx` | `.rg-spotlight` + `chart-demo.js` |
| Barras de tres capas | `src/app/org/charts.tsx`, `BarChart` | `.rg-bars`, `.rg-bar-*` |
| Barras apiladas y escala SHADES | `src/app/client/page.tsx` | `.rg-stacks`, tokens `stack-*` |
| Área de línea degradada | `src/app/org/charts.tsx`, `GrowthLine` | Tokens `chart.area-top` y `area-bottom`; receta abajo |
| Tipografía | `src/app/layout.tsx`, `globals.css` | Familias Epilogue, Sora y mono en los tokens |

Archivos listos para reutilizar:

- [`palette.json`](../assets/design/palette.json): fuente única editable.
- [`tokens.css`](../assets/design/tokens.css): variables generadas.
- [`centaury-recipes.css`](../assets/design/centaury-recipes.css): recetas sin framework.
- [`shadcn-theme.css`](../assets/design/shadcn-theme.css): puente al kit.
- [`chart-demo.js`](../assets/design/chart-demo.js): gráficos de muestra y spotlight.
- [`preview.html`](../assets/design/preview.html): muestrario con datos ficticios identificados.

## Tinta, degradados y superficies

Los títulos claros emplean el gradiente original de `foreground` hasta `primary`,
con paradas 45% y 140%. En oscuro, blanco hasta el pizarra claro de la marca, 40% y
135%. Ambas recetas recortan el fondo al texto; nunca se aplican a párrafos, tablas
ni logs. En colores forzados se recupera texto sólido legible.

La banda radial ilumina el centro superior y termina en el tono profundo de la
rampa; la lineal ilumina el borde superior. No sustituirlas por una tarjeta plana
negra ni por el antiguo azul hielo. El botón del login tiene su propio gradiente
vertical. Las definiciones exactas están en `gradient`, sin hex dispersos en componentes.

## Contenedores y bordes

- `Container.tsx` usa ancho máximo de 72rem y padding horizontal de 24px. Las
  variantes responsivas del informe son recetas de páginas, no propiedades del
  componente base.
- Shell de login: máximo 1180px, radio 28px, padding 16px y sombra larga muy suave.
  Fondo con cuadrícula de 56px y líneas pizarra al 3%.
- Radios: control 12px, tarjeta 16px, banda 24px, shell 28px, píldora completamente redonda.
- Borde claro: variable `border` de la raíz activa. Borde de banda: `navbar-border`.
  Un contorno que identifica un campo usa un tono más visible; no se confunde con
  un separador decorativo.
- Spotlight: borde de 1px con máscara `exclude`, radio luminoso de 240px; relleno
  de 420px. Las capas decorativas no interceptan eventos. Coordenadas mediante
  variables CSS, sin renderizar React por cada movimiento. Con movimiento reducido
  se mantiene una iluminación estática; en táctil no se sigue el cursor.

## Gráficos: el estilo es parte del componente

**Barras:** esquinas rectas, separación 3px, capuchón de 5px, franja blanca de 3px,
cuerpo pizarra que se desvanece. Opacidades superiores alternadas 0.26 / 0.50 y
final 0.02. Rejilla pizarra al 7%; tooltip oscuro, compacto. Una barra activa cambia
a tinta sólida conservando la altura correspondiente al dato, como `org/BarChart`.
No usar la columna marcadora a altura completa de la maqueta `/client` como dato.

**Apiladas:** seis tonos SHADES, tono más oscuro abajo, segmentos con radio 6px y
separación 4px. La leyenda lleva nombres, no solo muestras de color. Altura total
proporcional al total y segmentos proporcionales a sus valores.

**Líneas y área:** SVG, trazo pizarra de 2px con `vector-effect="non-scaling-stroke"`;
relleno de opacidad 0.22 a 0. Al convertirlo en componente, generar un ID de gradiente
único por instancia: el `growth-fill` fijo del origen no debe duplicarse. Etiquetar
los datos también fuera del SVG y ofrecer foco/teclado para puntos interactivos.

**Medidores:** pista redonda del color tinta al 10%, relleno pizarra. Ámbar solo cuando
el estado lo justifique. Normalizar denominador, controlar valores vacíos, negativos
y no finitos al conectarlos a datos reales.

Las series normales usan la rampa monocroma, **no** los colores verde/rojo de estado.
La muestra incluye barras seleccionables por teclado y apiladas etiquetadas. La
receta de línea y medidor queda documentada; no se ha instalado una biblioteca de
gráficos ni creado un dashboard de aplicación.

## Decoración, tipografía y movimiento

ASCII abstracto de 2–4 líneas, 10–11px, monoespaciado, `aria-hidden` si es decorativo.
Epilogue para encabezados y Sora para cuerpo, como la web. Las fuentes aún no se
empaquetan aquí; las alternativas locales funcionan sin red.

La curva común es `cubic-bezier(.16,1,.3,1)`. Nada de animación constante en logs.
El origen tiene shaders ASCII y seda; sus reglas son: importar de forma diferida,
DPR limitado, pausar fuera de pantalla, liberar recursos y mostrar el canvas solo
cuando hay textura. Se documentan, pero no se importa Three.js para una muestra de
estilos. La decoración estática mantiene esta versión ligera.

## Integración y comprobación

Cargar `tokens.css`, después `centaury-recipes.css`; para Tailwind, añadir también
el adaptador de shadcn. Las clases `.rg-*` son independientes y se pueden usar por
separado. En la app futura los overrides de variables se pueden aplicar en ejecución;
el JSON es la configuración maestra de este paquete, no un gestor de temas ya implementado.

Ejecutar `python3 assets/design/build_tokens.py` tras editar la paleta. Comprueba los
pares de texto y contornos usados en paneles. No afirma cubrir gradientes, shaders ni
todos los estados de una aplicación que aún no está implementada.
