# IDEA — TOON como formato de tokens

> **Estado: ❓ SIN DEFINIR — documento en progreso.** Continuación pendiente.
> Este documento **no** es un diseño. Es el sitio donde vive la idea mientras se define.
> **No implementes nada de aquí todavía.**
> Nace de la conversación del 2026-10-02 con Alexander: *"en vez de JSON usamos TOON […]
> cómo hacemos que la AI lo use nativamente, sin ponerlo en system prompt ni nada […]
> sin hacer harness."*

---

## 1. La idea, en palabras de Alexander

> "Quiero que Regulus tenga un módulo en el cual básicamente en vez de JSON usamos TOON.
> ¿Y cómo hacemos que la AI, sin usar su contexto, lo use nativamente — sin ponerlo en
> system prompt ni nada, explicándole las reglas? Apenas esta piense en JSON
> necesitaremos un sistema que se lo comente. Lo mejor que yo veo es basarnos en papers."

Tres requisitos, separables:

1. **TOON como formato de intercambio** en lugar de JSON donde convenga.
2. **Cero *prompt tax***: sin instrucciones de formato ni ejemplos en cada petición.
3. **Sin harness**: que sea una pieza simple de optimización, no un framework de agentes.

**TOON:** Token-Oriented Object Notation — codificación compacta y sin pérdida del modelo
de datos de JSON, con formas tabulares para arrays uniformes y longitudes declaradas
(`[N]`, `{fields}`). Referencia: <https://toonformat.dev>.

---

## 2. La respuesta dura: no hay tercera vía

Un modelo congelado solo **genera** TOON si ocurre una de estas tres cosas:

| # | Mecanismo | ¿Prompt tax? | Coste |
|---|---|---|---|
| 1 | **Instrucción en contexto** (ICL) | Sí | Tokens fijos en cada petición |
| 2 | **Constrained decoding** (gramática máscara logits) | No | Requiere controlar el stack de inferencia; puede degradar el modelo |
| 3 | **Fine-tune / LoRA** | No | Pesado; no es un módulo de Regulus, es un proyecto de modelo |

No existe una cuarta. "Nativo sin prompt" solo es posible con 2 o 3.

### 2.1 Evidencia

**arXiv 2603.03306** — *TOON vs JSON: A Benchmark of Plain and Constrained Decoding
Generation* (Ivan Matveev, feb 2026). El caso exacto. Hallazgos:

- **Prompt tax:** TOON solo amortiza la instrucción cuando el payload es grande. En
  `users` (tabular plano) gana: 22% menos tokens, 90.5% one-shot. En contextos cortos
  pierde contra JSON plano (Qwen3-235B: 4715 vs 2772 tokens totales).
- **Topología manda:** colapsa en jerarquías profundas/no uniformes (caso `company`:
  0% one-shot). Brilla en tabular y anidado+uniforme (caso `order`: 78.6%).
- **El paper deja abierto el experimento que esta idea necesita:** meter la gramática
  TOON en un state machine tipo XGrammar. Su hipótesis es que *puede no dar el resultado
  esperado* — para tablas simples, JSON con structured output gastó menos (556 vs 840).
- **Riesgo adicional:** *indentation drift* en generaciones largas (TOON depende de
  whitespace).

**Contra el enforcement estricto:**

- **arXiv 2408.02442** — *Let Me Speak Freely?* (Tam et al., 2024): restricciones de
  formato estrictas degradan razonamiento; lo mejoran en clasificación.
- **Schall & de Melo 2025** (RANLP) — *The Hidden Cost of Structure*: constrained
  decoding interfiere con el desempeño del modelo.
- El propio benchmark: Hermes-4-405B cae 92.5% → 35% one-shot con constrained decoding.
  Ayuda a modelos débiles (Qwen2.5-Coder-7B: 0% → 75%), perjudica a los fuertes.

**A favor de TOON como entrada (no generación):**

- TOON comprehension: 72.2% accuracy vs 71.4% JSON, con 42.6% menos tokens
  (benchmarks de toonformat.dev, 244 preguntas, 4 modelos).
- Thoughtworks lo cataloga como *"last-mile optimization for prompt inputs"*, no como
  reemplazo de JSON en APIs ni en outputs de modelo.

---

## 3. La distinción que lo desbloquea: entrada ≠ salida

La pregunta de Alexander ("cómo hacer que la AI lo use nativamente") trata la
generación. Pero el ahorro grande y **gratis** está en la otra dirección:

| Dirección | Qué se necesita | Prompt tax | ¿Resuelto? |
|---|---|---|---|
| **JSON → TOON → contexto del modelo** (entrada) | Función determinista. El modelo solo *lee*. | **Cero** | Sí, hoy |
| **Modelo → TOON** (salida) | Cooperación del modelo o enforcement en decoding | Sí / condicionado | ❌ Abierto |

- **Entrada:** el modelo no necesita "saber" TOON para leerlo — la evidencia muestra que
  lo comprende. JSON se queda como formato de almacenamiento/API; TOON entra al contexto.
  Esta es exactamente la postura de la spec TOON ("use JSON programmatically, encode it
  as TOON for LLM input").
- **Salida:** default JSON (donde el modelo es más preciso) → transcode a TOON para
  almacenar/transportar. Enforcement por gramática **solo** si el stack lo permite y el
  payload lo justifica.

---

## 4. Lectura para Regulus (🔶 PROPUESTO — sin aprobar)

1. **No hay reemplazo global de JSON.** Hay un **router por payload** que decide formato
   según topología (tabular/uniforme vs anidado) y tamaño (amortización del prompt tax).
   Los umbrales se miden, no se inventan.
2. **Códec + router = dos funciones puras y una política.** No hay bucle de agente, no
   hay orquestación. Eso es lo que lo mantiene fuera del territorio harness.
3. **Encaja como módulo de capacidad**, según [`../03-modules.md`](../03-modules.md):
   algo como `format.encode` / `format.decode`, opcional (`required = false`). El core
   no lo conoce por nombre. Verificar contra la matriz de ablación cuando exista.
4. **La matriz de ablación no se ve afectada:** sin el módulo, todo habla JSON. Es el
   caso ideal de `required = false`.

---

## 5. La pregunta que decide el diseño

❓ **¿Regulus va a controlar el stack de inferencia o solo va a hablar con APIs
externas?**

- **Solo APIs** (OpenAI, Anthropic, etc.): el mecanismo 2 no está disponible salvo donde
  el proveedor exponga grammars/structured outputs. El diseño correcto es
  **JSON-interchange + TOON-context**.
- **Controla decoding** (llama.cpp / vLLM / SGLang locales): se puede intentar el
  experimento que arXiv 2603.03306 dejó abierto (gramática TOON vía XGrammar/llguidance),
  asumiendo el riesgo de degradación en modelos fuertes.

---

## 6. Preguntas abiertas

1. **Stack de inferencia** (§5). Bloquea todo lo demás.
2. **¿Router con qué señal?** ¿Elegibilidad tabular calculada sobre el payload? ¿Tamaño
   en tokens? ¿Clase de modelo? Los umbrales del paper son por caso, no universales.
3. **¿La UI/shell muestra el formato elegido?** El requisito 5 de
   [`../01-vision.md` §5](../01-vision.md) pide que el shell siempre diga qué está
   corriendo. ¿Aplica a esto?
4. **¿Se mide?** El principio "Rápido" exige medir contra presupuesto
   ([`../00-principles.md`](../00-principles.md)). Sin benchmark propio (payloads reales
   de Alexander), los números del paper no son evidencia local.
5. **¿Dónde vive el códec: en `regulus-proto` o en un módulo?** Si los clientes también
   quieren TOON, toca el protocolo del límite externo.

---

## 7. Lo único que ya se puede afirmar

- **TOON no reemplaza a JSON en todas partes.** El paper lo demuestra para generación;
  Thoughtworks lo repite para APIs/DBs. Cualquier diseño que asuma reemplazo global está
  contradiciendo la evidencia.
- **El enforcement estricto no es gratis.** Aplicar gramática TOON a un modelo fuerte
  puede costar más precisión de la que ahorra en tokens. No se activa por defecto.
- **La ganancia de la entrada es real y sin condiciones de modelo.** Codificar a TOON lo
  que entra al contexto ahorra tokens sin cooperación del modelo.
- **Esto no necesita ser un harness.** Códec, router, y a lo sumo un logit processor
  cuando el stack lo permita.

---

## 8. Referencias

| Trabajo | Qué aporta |
|---|---|
| [arXiv 2603.03306](https://arxiv.org/abs/2603.03306) — TOON vs JSON generation benchmark (2026) | Prompt tax, topología, JSO, xgrammar como trabajo futuro |
| [arXiv 2411.15100](https://arxiv.org/abs/2411.15100) — XGrammar (Dong et al., 2024) | Motor de constrained decoding de referencia |
| [arXiv 2408.02442](https://arxiv.org/abs/2408.02442) — Let Me Speak Freely? (Tam et al., 2024) | Restricciones estrictas degradan razonamiento |
| Schall & de Melo 2025 (RANLP) — The Hidden Cost of Structure | Efecto del constrained decoding en desempeño |
| [toonformat.dev](https://toonformat.dev) · [spec](https://github.com/toon-format/spec) | Especificación TOON y benchmarks de comprensión |
| [Thoughtworks Radar — TOON](https://www.thoughtworks.com/en-us/radar/languages-and-frameworks/toon-token-oriented-object-notation) | Adopción: optimización de entrada, no reemplazo de JSON |

---

## Historial

| Fecha | Cambio |
|---|---|
| 2026-10-02 | Nace el documento. Evidencia recogida, diseño sin decidir. Pendiente: §5 (stack de inferencia) y §6. |
