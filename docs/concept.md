# Qué es Regulus — la idea

> **v0.3** · 2026-10-05 · La idea de Alexander, en llano. El diseño técnico está en
> [`11-composition.md`](11-composition.md) (✅). Qué es base y qué es módulo, y en qué
> orden se construye: [`plan.md`](plan.md).

---

## 1. En una frase

**Regulus es una base para conectar herramientas, como un lego pero rápido.** Instalas
módulos, conectas sus piezas, y esa forma de conectarlas decide cómo trabaja el sistema.

---

## 2. La IA es el centro, pero está en módulos

- **Para qué sirve:** sin IA no hace nada útil. Es el centro.
- **Cómo está hecho:** la base no tiene funciones propias; solo encaja piezas. Todo lo
  útil es un módulo.
- **Conjunto base:** viene de serie con los módulos imprescindibles (comunicación,
  agente, proveedores, modelos, procesos). Se pueden quitar o cambiar por otros.

Como Linux: el kernel solo no sirve para nada; lo útil son los paquetes que trae.

---

## 3. Las piezas

```
Perfil  (global, o propio de un proyecto)
 ├─ Workflow principal
 ├─ Workflows extra (cada uno en su canvas)
 └─ Workflow de cada módulo
       └─ Nodos
```

| Palabra | Qué es |
|---|---|
| **Módulo** | Paquete instalable hecho para Regulus. Trae nodos y su propio workflow |
| **Nodo** | Una pieza pequeña que trae un módulo: "Agente", "OpenAI", "Enviar mensaje" |
| **Recurso** | Un fichero que se conecta como un nodo: skill, research, prompt |
| **Cable "usa"** | Esta pieza usa a esa (Agente ⇢ OpenAI) |
| **Cable "luego"** | Cuando pasa esto, haz aquello (Mensaje recibido → Entregar) |
| **Workflow** | Una cadena de funcionamiento: qué nodos hay y cómo se conectan |
| **Perfil** | Todos los workflows y ajustes con los que trabaja un proyecto |
| **Plantilla** | Un workflow o perfil guardado para reutilizarlo |
| **Ejecución** | Un workflow en marcha. Puede haber varias a la vez |

---

## 4. Cómo funciona

1. **Cada módulo trae su workflow ya armado** y funciona sin tocar nada. Lo puedes ver
   entero y cambiar lo que el módulo permita.
2. **Puedes ver todos los workflows juntos** en un solo canvas, o cada uno en el suyo.
3. **Cada proyecto tiene su perfil:** hereda del global y cambia lo suyo, o empieza de
   cero.
4. **Varios proyectos corren a la vez**, cada uno con su configuración, aislados entre
   sí.
5. **Las claves (API keys) van aparte**, nunca dentro de un workflow. Compartir una
   plantilla no comparte tus claves.

---

## 5. Dos vistas

| Vista | Para qué |
|---|---|
| **Canvas** | Diseñar: nodos y cables |
| **Operar** | Ver lo que corre ahora: agentes, terminales, logs, errores |

---

## 6. Lo que tiene que ser verdad siempre

1. **Quitar una pieza no rompe las demás.**
2. **Funciona por defecto**, sin configurar nada.
3. **Todo se cambia sin programar:** canvas, fichero o terminal.
4. **Rápido**, con números medidos ([`00-principles.md`](00-principles.md)).
5. **Toda pieza es una caja líquida:** por fuera tiene una forma fija; por dentro se le
   cambia el motor, se le conecta lo que quieras en cada ranura, se envuelve, se abre y
   se guarda como plantilla ([`00-principles.md`](00-principles.md)).

> *"Nuestro producto tiene que ser como el agua."* — Alexander
