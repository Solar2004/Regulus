# La mascota de Regulus Code

> 2026-11-09 · Referencia compartida por la app (SVG) y la terminal (bloques). La fuente
> de verdad es [`regulus-code-mascot.json`](regulus-code-mascot.json); este documento la
> explica. Si los dos se contradicen, manda el JSON.

## Qué es

Un león pequeño visto de frente, dibujado en una rejilla de píxeles. Regulus es α Leonis,
la estrella del corazón de Leo (*Cor Leonis*), y de ahí salen sus tres rasgos:

1. **La melena tiene puntas de estrella.** El contorno es dentado, con una punta en cada
   eje y en cada diagonal.
2. **La cara tiene forma de corazón.** El mechón que baja entre las orejas parte la
   frente en dos lóbulos y la barbilla cierra en punta.
3. **Lleva una estrella en la frente:** un solo píxel claro, donde acaba el mechón.

Es un dibujo propio. No copia la mascota de ningún otro producto: no hay cuerpo, ni
patas, ni la silueta ni los colores de otra.

## La rejilla

`full`, 15 × 14. Es la que se usa siempre que quepa.

```
     0 1 2 3 4 5 6 7 8 9 . . . . 14
 0   . . . . . . . M . . . . . . .
 1   . . M . . M M M M M . . M . .
 2   . . . M M M F M F M M M . . .
 3   . M M M F F F M F F F M M M .
 4   . . M M F F F S F F F M M . .
 5   M M M F F F F F F F F F M M M
 6   . . M F E F F F F F E F M . .
 7   . M M F E F W W W F E F M M .
 8   M M M F F W N N N W F F M M M
 9   . . M M F W W N W W F M M . .
10   . M M M F F W W W F F M M M .
11   . . M M M M F F F M M M M . .
12   . . M . M M M M M M M . M . .
13   . . . . . . . M . . . . . . .
```

`mini`, 9 × 8, para sitios donde la grande no cabe (una barra, un título):

```
     0 1 2 3 4 5 6 7 8
 0   . . . . M . . . .
 1   . M . M M M . M .
 2   . . M F M F M . .
 3   M M F F S F F M M
 4   . M E F F F E M .
 5   M M F W N W F M M
 6   . M M F W F M M .
 7   . . M . M . M . .
```

Las dos son simétricas respecto a la columna central; `build_mascot.py` lo comprueba.

## Papeles y colores

Cada letra es un papel, y cada papel toma un color de
[`palette.json`](palette.json). Nunca se escribe un color a mano.

| Letra | Papel | Token | Para qué |
|---|---|---|---|
| `M` | melena | `steel` | el contorno dentado |
| `F` | cara | `focus` | el corazón |
| `W` | hocico | `text` | la zona clara bajo los ojos |
| `S` | estrella | `text` | el píxel de la frente |
| `E` | ojo | `canvas` | se ve como un hueco sobre el fondo |
| `N` | nariz | `canvas` | la T del hocico |
| `.` | vacío | — | transparente |

En la terminal los mismos papeles salen de `assets/design/palette.json`, como el resto de
colores de la TUI (`docs/04-client.md` §7). Con menos de cuatro colores disponibles,
`W` y `S` pueden usar el color de `F`; con uno solo, se dibuja la silueta.

## Poses

Solo cambian los ojos. Cada pose es la lista de celdas `[fila, columna]` que son ojo; las
demás celdas de ojo de la rejilla pasan a ser cara.

| Pose | Rejilla `full` | Cuándo |
|---|---|---|
| `idle` | ojos de 1 × 2 en las columnas 4 y 10 | en reposo |
| `blink` | un píxel por ojo, fila 7 | parpadeo, un instante |
| `left` / `right` | los ojos una columna a la izquierda o a la derecha | trabajando: mira lo que lee |
| `sleep` | dos píxeles en horizontal por ojo, fila 7 | sesión parada |

`mini` tiene `idle`, `blink` y `sleep` (las dos últimas sin ojos). Una pose que una rejilla
no tiene se dibuja como `idle`.

La estrella de la frente puede tomar el color de un estado (`warning` cuando la sesión
espera una respuesta, `danger` si falló). **La mascota nunca es el único indicador de un
estado:** al lado va siempre el icono y la palabra de `docs/08-design-system.md`.

## En la terminal

Con medios bloques cada carácter lleva dos píxeles, el de arriba y el de abajo, así que
`full` ocupa 15 columnas × 7 líneas y `mini` 9 × 4. Cada celda se pinta con `▀` y dos
colores: el de primer plano para el píxel de arriba y el de fondo para el de abajo (`▄` si
solo hay el de abajo, un espacio si no hay ninguno). La silueta, sin colores, queda así:

```
  ▄  ▄▄█▄▄  ▄            ▄ ▄█▄ ▄
 ▄▄█████████▄▄          ▄▄█████▄▄
▄▄███████████▄▄         ▄███████▄
 ▄███████████▄           ▀█▀█▀█▀
▀▀███████████▀▀
 ▀███████████▀
  ▀ ▀▀▀█▀▀▀ ▀
```

La terminal y la app no comparten código: cada una lleva su copia de la rejilla. Para que
no se separen, la app tiene un test que compara su componente con el JSON
(`frontend/src/code/mascot.test.ts`); la terminal puede hacer lo mismo leyendo el JSON en
un test.

## Reglas de uso

- Se escala por enteros: cada píxel de la rejilla es un cuadrado de N × N píxeles de
  pantalla, sin suavizado (`shape-rendering="crispEdges"` en el SVG).
- No se gira, no se deforma y no se le cambia el color por papel.
- Movimiento: solo el cambio de pose, a saltos. Sin brillos ni fundidos, y quieta con
  `prefers-reduced-motion`.
- Es la marca de Regulus Code. No sustituye a los iconos de la interfaz, que siguen
  siendo de Lucide.

## Regenerar

```sh
python3 assets/design/build_mascot.py          # escribe regulus-code-mascot.svg
python3 assets/design/build_mascot.py --check  # falla si el SVG no sale del JSON
```

El generador valida las rejillas: filas del mismo ancho, simetría, papeles conocidos y
poses que solo ponen ojos dentro de la cara.
