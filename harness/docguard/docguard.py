#!/usr/bin/env python3
"""docguard — comprueba que lo que hace un agente sigue la documentación del repo.

Cuatro capas, de barata a cara (el porqué de cada una está en docs/10-docguard.md):

  1. `deny`     antes de actuar: bloquea acciones prohibidas (push, --no-verify…). 0 tokens.
  2. `check`    al terminar: corre los comandos de verificación (tipos, tests) de las
                rutas tocadas. Si fallan, el agente no para.                      0 tokens.
  3. `forbid`   al terminar: regex sobre las líneas añadidas del diff.            0 tokens.
  4. `semantic` al terminar, solo si 2-3 pasan y alguna regla aplica a las rutas
                tocadas: un juez headless (opencode) audita el diff contra esas reglas.

Y la capa `decide`: cuando el agente elige entre caminos (o se los presenta al usuario) sin
contrastarlos con docs/00-principles.md, se le para una vez con el checklist de principios.

Además, la primera vez que el agente toca una ruta recibe solo las reglas de esa ruta
(contexto justo a tiempo), en vez de un prompt gigante al principio. Y si edita el mismo
fichero muchas veces seguidas, recibe un aviso para que replantee (detector de bucles).

Uso:
  docguard.py check [--session ID]     revisa ahora, como haría el hook (exit 1 = fallos)
  docguard.py rules [RUTA]             reglas que aplican a una ruta
  docguard.py compile [--force]        regenera las reglas `compiled` desde los docs
  docguard.py reset [--session ID]     olvida el estado de una sesión (o de todas)
  docguard.py decide [--tool]          ¿pararía `decide` este texto? (stdin; exit 1 = sí)
  docguard.py eval [--no-judge]        mide precisión y recall contra el gold set (eval/cases)
  docguard.py hook claude|opencode     adaptador de hooks: JSON por stdin, JSON por stdout

Sin dependencias: Python 3.9+ y git. El juez necesita `opencode` en el PATH.
"""

from __future__ import annotations

import difflib
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
CONF_FILE = Path(os.environ.get("DOCGUARD_CONF", HERE / "docguard.conf"))
RULES_FILE = Path(os.environ.get("DOCGUARD_RULES", HERE / "rules.json"))
PROMPTS = HERE / "prompts"

# Valores por defecto: docguard.conf los sobreescribe. Ningún literal más abajo.
DEFAULTS = {
    "enabled": "true",
    "judge_model": "opencode/muse-spark-1.3-contributor-free",
    "judge_agent": "plan",
    "judge_timeout": "240",
    "judge_verify": "true",
    "max_rounds": "2",
    "max_findings": "5",
    "max_diff_bytes": "24000",
    "judge_max_calls": "6",
    "attribution": "touched",
    "jit_context": "true",
    "always_loaded": "AGENTS.md CLAUDE.md",
    "judge_rules_per_call": "15",
    "jit_max_chars": "1500",
    "ignore_paths": "",
    "compile_sources": "AGENTS.md",
    "compile_heading_level": "2",
    "compile_model": "opencode/muse-spark-1.3-contributor-free",
    "check_timeout": "300",
    "check_output_chars": "1500",
    "loop_edits": "8",
    "eval_dir": "eval/cases",
    "decide": "true",
    "decide_tools": "",
    "decide_markers": "",
    "principle_markers": "",
}

KINDS = ("deny", "forbid", "semantic")
DELETED = "deleted"


# --- Configuración ------------------------------------------------------------------

def load_conf() -> dict[str, list[str]]:
    conf: dict[str, list[str]] = {}
    if CONF_FILE.exists():
        for raw in CONF_FILE.read_text(encoding="utf-8").splitlines():
            line = re.sub(r"\s+#.*$|^#.*$", "", raw).strip()
            if "=" not in line:
                continue
            key, val = (s.strip() for s in line.split("=", 1))
            if val:
                conf.setdefault(key, []).append(val)
    return conf


CONF = load_conf()


def cfg(key: str) -> str:
    vals = CONF.get(key)
    return vals[0] if vals else DEFAULTS[key]


def cfg_list(key: str) -> list[str]:
    vals = CONF.get(key) or [DEFAULTS[key]]
    return " ".join(vals).split()


def cfg_bool(key: str) -> bool:
    return cfg(key).lower() in ("1", "true", "yes", "on")


def cfg_int(key: str) -> int:
    return int(cfg(key))


# --- Git ----------------------------------------------------------------------------

def git(*args: str, check: bool = True, input_: str | None = None) -> str:
    r = subprocess.run(["git", *args], cwd=ROOT, capture_output=True, text=True, input=input_)
    if check and r.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)}: {r.stderr.strip()}")
    return r.stdout


def find_root() -> Path:
    r = subprocess.run(["git", "rev-parse", "--show-toplevel"], cwd=HERE,
                       capture_output=True, text=True)
    return Path(r.stdout.strip()) if r.returncode == 0 else HERE.parent.parent


ROOT = find_root()


def state_dir() -> Path:
    d = Path(git("rev-parse", "--git-common-dir").strip())
    d = (d if d.is_absolute() else ROOT / d) / "docguard"
    d.mkdir(parents=True, exist_ok=True)
    return d


def head() -> str:
    return git("rev-parse", "--verify", "-q", "HEAD", check=False).strip()


def dirty_files(base: str) -> list[str]:
    """Ficheros que difieren de `base` (incluye commits hechos desde entonces) + sin trackear."""
    files = set()
    if base:
        files.update(git("diff", "--name-only", "-z", base).split("\0"))
    else:
        files.update(git("ls-files", "-z").split("\0"))
    files.update(git("ls-files", "--others", "--exclude-standard", "-z").split("\0"))
    return sorted(f for f in files if f and not ignored(f))


def hash_files(paths: list[str], write: bool = False) -> dict[str, str]:
    """Hash git de cada fichero existente. `write=True` guarda el blob para poder diffear luego."""
    existing = [p for p in paths if (ROOT / p).is_file()]
    if not existing:
        return {}
    args = ["hash-object", "--stdin-paths"] + (["-w"] if write else [])
    out = git(*args, input_="\n".join(existing) + "\n").split()
    return dict(zip(existing, out))


def old_content(path: str, base: str, baseline: dict[str, str]) -> str:
    if baseline.get(path) == DELETED:
        return ""
    if path in baseline:
        return git("cat-file", "-p", baseline[path], check=False)
    if base:
        return git("show", f"{base}:{path}", check=False)
    return ""


# --- Rutas y reglas -----------------------------------------------------------------

def glob_re(pattern: str) -> re.Pattern[str]:
    """Glob con `**`. Un patrón sin `/` casa con el nombre en cualquier carpeta."""
    p = os.path.expanduser(pattern)
    out, i = "", 0
    if "/" not in p:
        out = "(?:.*/)?"
    while i < len(p):
        if p.startswith("**/", i):
            out, i = out + "(?:.*/)?", i + 3
        elif p.startswith("**", i):
            out, i = out + ".*", i + 2
        elif p[i] == "*":
            out, i = out + "[^/]*", i + 1
        elif p[i] == "?":
            out, i = out + "[^/]", i + 1
        else:
            out, i = out + re.escape(p[i]), i + 1
    return re.compile(out + r"\Z")


def matches(path: str, patterns: list[str]) -> bool:
    return any(glob_re(p).match(path) for p in patterns)


def ignored(path: str) -> bool:
    return matches(path, cfg_list("ignore_paths"))


def load_rules() -> list[dict]:
    if not RULES_FILE.exists():
        return []
    data = json.loads(RULES_FILE.read_text(encoding="utf-8"))
    return [r for r in data.get("rules", []) if r.get("enabled", True) and r.get("kind") in KINDS]


def rule_applies(rule: dict, path: str) -> bool:
    return matches(path, rule.get("paths") or ["**"]) and not matches(path, rule.get("exclude") or [])


def rel(path: str) -> str:
    """Ruta relativa al repo si está dentro; absoluta si está fuera."""
    p = Path(os.path.expanduser(path))
    p = p if p.is_absolute() else ROOT / p
    try:
        return str(p.resolve().relative_to(ROOT))
    except ValueError:
        return str(p.resolve())


# --- Estado por sesión --------------------------------------------------------------

def session_path(sid: str) -> Path:
    safe = re.sub(r"[^A-Za-z0-9_.-]", "_", sid or "manual")
    return state_dir() / f"{safe}.json"


def load_session(sid: str) -> dict:
    """Carga el estado. Si no existe, toma la foto de partida: lo que ya estaba cambiado
    antes de que el agente empezara no es suyo y no se le revisa."""
    p = session_path(sid)
    if p.exists():
        return json.loads(p.read_text(encoding="utf-8"))
    base = head()
    dirty = dirty_files(base)
    baseline = {f: DELETED for f in dirty} | hash_files(dirty, write=True)
    st = {"base": base, "baseline": baseline,
          "reviewed": {}, "rounds": 0, "injected": [], "pending": [], "touched": []}
    save_session(sid, st)
    return st


def save_session(sid: str, st: dict) -> None:
    session_path(sid).write_text(json.dumps(st, indent=1), encoding="utf-8")


# --- Diff ---------------------------------------------------------------------------

def changed_since(st: dict) -> list[str]:
    """Ficheros que el agente ha cambiado y que nadie ha revisado todavía en ese estado."""
    files = sorted(set(dirty_files(st["base"])) | set(st["baseline"]))
    # Con varios agentes en el mismo árbol, git no sabe quién cambió qué: si la sesión
    # registró las rutas que editó, solo se le revisan esas (attribution = touched).
    if cfg("attribution") == "touched" and st.get("touched") is not None and not st.get("manual"):
        files = [f for f in files if f in set(st["touched"])]
    now = hash_files(files)
    out = []
    for f in files:
        h = now.get(f, DELETED)
        if h == st["baseline"].get(f) or h == st["reviewed"].get(f):
            continue
        if h == DELETED and f not in st["baseline"] and not exists_at(st["base"], f):
            continue
        out.append(f)
    return out


def exists_at(rev: str, path: str) -> bool:
    return bool(rev) and subprocess.run(["git", "cat-file", "-e", f"{rev}:{path}"], cwd=ROOT,
                                        capture_output=True).returncode == 0


def file_diff(path: str, st: dict) -> tuple[str, list[tuple[int, str]]]:
    """Diff unificado de un fichero y sus líneas añadidas como (número, texto)."""
    old = old_content(path, st["base"], st["baseline"]).splitlines()
    fp = ROOT / path
    try:
        new = fp.read_text(encoding="utf-8").splitlines() if fp.is_file() else []
    except UnicodeDecodeError:
        return "", []
    return make_diff(path, old, new)


def make_diff(path: str, old: list[str], new: list[str]) -> tuple[str, list[tuple[int, str]]]:
    diff = list(difflib.unified_diff(old, new, f"a/{path}", f"b/{path}", lineterm="", n=2))
    added, ln = [], 0
    for line in diff:
        m = re.match(r"@@ -\d+(?:,\d+)? \+(\d+)", line)
        if m:
            ln = int(m.group(1))
        elif line.startswith("+") and not line.startswith("+++"):
            added.append((ln, line[1:]))
            ln += 1
        elif not line.startswith("-"):
            ln += 1
    return "\n".join(diff), added


# --- Capa 1: acciones prohibidas ----------------------------------------------------

def guard(tool: str, command: str | None, path: str | None) -> dict | None:
    """Primera regla `deny` que prohíbe la acción, o None."""
    for r in load_rules():
        if r["kind"] != "deny":
            continue
        if r.get("tool") == "bash" and command and re.search(r["pattern"], command):
            return r
        if r.get("tool") == "edit" and path and matches(rel(path), r.get("paths") or []):
            return r
    return None


def deny_message(r: dict) -> str:
    return (f"docguard [{r['id']}] {r.get('source', '')}\n"
            f"Regla: «{r['text']}»\n"
            + (r.get("hint") or "Si de verdad hace falta, pídeselo al usuario."))


# --- Contexto justo a tiempo --------------------------------------------------------

def touch(sid: str, path: str) -> None:
    """Anota que esta sesión editó `path` (atribución con varios agentes en el mismo árbol)."""
    st, p = load_session(sid), rel(path)
    if p not in st.setdefault("touched", []):
        st["touched"].append(p)
        save_session(sid, st)


def loop_notice(sid: str, path: str) -> str | None:
    """Cuenta las ediciones de cada fichero en la sesión. Al llegar a `loop_edits` avisa una
    vez: editar lo mismo una y otra vez suele ser un bucle sin salida (docs/10-docguard.md)."""
    limit = cfg_int("loop_edits")
    if limit <= 0:
        return None
    st, p = load_session(sid), rel(path)
    edits = st.setdefault("edits", {})
    edits[p] = edits.get(p, 0) + 1
    save_session(sid, st)
    if edits[p] != limit:
        return None
    return (f"docguard: llevas {limit} ediciones de {p} en esta sesión. Si estás dando vueltas, "
            "para antes de la siguiente: relee el error completo, revisa la hipótesis o pregunta "
            "al usuario.")


def join_ctx(*parts: str | None) -> str | None:
    text = "\n\n".join(p for p in parts if p)
    return text or None


def specificity(r: dict) -> tuple[int, int]:
    """Menor = más específica: reglas acotadas a pocas rutas concretas antes que las de `**`."""
    paths = r.get("paths") or ["**"]
    return (sum(p.count("**") for p in paths), len(paths))


def jit(sid: str, path: str) -> str | None:
    if not cfg_bool("jit_context"):
        return None
    st = load_session(sid)
    p = rel(path)
    loaded = cfg_list("always_loaded")
    new = [r for r in load_rules()
           if r["kind"] != "deny" and r["id"] not in st["injected"] and rule_applies(r, p)
           # Lo que viene de un fichero que el agente ya tiene en contexto sería un duplicado.
           and r.get("source", "").split(" §")[0] not in loaded]
    new.sort(key=specificity)  # con presupuesto limitado, primero lo más propio de esta ruta
    if not new:
        return None
    lines, budget = [], cfg_int("jit_max_chars")
    for r in new:
        item = f"- [{r['id']}] {r['text']} ({r.get('source', '')})"
        if sum(len(x) for x in lines) + len(item) > budget:
            break
        lines.append(item)
        st["injected"].append(r["id"])
    save_session(sid, st)
    return f"docguard — reglas de la documentación que aplican a {p}:\n" + "\n".join(lines)


# --- Capa 2: comandos de verificación -----------------------------------------------

def check_specs() -> list[tuple[list[str], str]]:
    """Entradas `check = <globs> :: <comando>` de docguard.conf."""
    out = []
    for raw in CONF.get("check", []):
        if "::" in raw:
            globs, cmd = (x.strip() for x in raw.split("::", 1))
            if globs and cmd:
                out.append((globs.split(), cmd))
    return out


def run_checks(files: list[str]) -> tuple[list[dict], str]:
    """Corre cada comando cuyas rutas casen con algún fichero cambiado. Un comando que falla
    es un hallazgo; uno que no termina a tiempo no bloquea: es un fallo de infraestructura."""
    out, slow = [], []
    for globs, cmd in check_specs():
        hit = [f for f in files if matches(f, globs)]
        if not hit:
            continue
        try:
            r = subprocess.run(cmd, shell=True, cwd=ROOT, capture_output=True, text=True,
                               timeout=cfg_int("check_timeout"), stdin=subprocess.DEVNULL)
        except subprocess.TimeoutExpired:
            slow.append(cmd)
            continue
        if r.returncode != 0:
            tail = (r.stdout + r.stderr).strip()[-cfg_int("check_output_chars"):]
            out.append({"rule": "CHECK", "file": hit[0], "line": None, "command": cmd,
                        "evidence": tail, "problem": "", "fix": ""})
    notice = "".join(f"docguard: `{c}` superó check_timeout y no se comprobó.\n" for c in slow)
    return out, notice.strip()


# --- Capa 3: reglas deterministas ---------------------------------------------------

def forbid_findings(diffs: dict[str, tuple[str, list]]) -> list[dict]:
    """Una línea que rompe varias reglas parecidas da un solo aviso (las manuales van primero)."""
    out, seen = [], set()
    for r in load_rules():
        if r["kind"] != "forbid":
            continue
        rx = re.compile(r["pattern"])
        for f, (_, added) in diffs.items():
            if not rule_applies(r, f):
                continue
            for ln, text in added:
                if rx.search(text) and (f, ln) not in seen:
                    seen.add((f, ln))
                    out.append({"rule": r["id"], "file": f, "line": ln, "evidence": text.strip(),
                                "problem": r.get("hint") or "", "fix": ""})
    return out


# --- Capa 4: juez semántico ---------------------------------------------------------

def run_opencode(model: str, prompt: str, agent: str) -> str:
    if not shutil.which("opencode"):
        raise RuntimeError("opencode no está en el PATH")
    with tempfile.NamedTemporaryFile("w", suffix=".md", delete=False, encoding="utf-8") as tf:
        tf.write(prompt)
    # `-f` es un array y se traga lo que venga detrás: el mensaje va primero, `-f` al final.
    cmd = ["opencode", "run", "Follow the instructions in the attached file. Reply with the requested JSON only.",
           "--pure", "-m", model, "--agent", agent, "-f", tf.name]
    env = dict(os.environ, DOCGUARD_JUDGE="1")  # el plugin de opencode no se dispara a sí mismo
    # El juez corre en un directorio vacío: `plan` es de solo lectura por su prompt, no por
    # permisos, y así aunque intentara escribir no tocaría el repo. Todo lo que necesita
    # (reglas y diff) va en el fichero adjunto.
    sandbox = tempfile.mkdtemp(prefix="docguard-")
    cmd += ["--dir", sandbox]
    try:
        r = subprocess.run(cmd, cwd=sandbox, capture_output=True, text=True, env=env,
                           timeout=cfg_int("judge_timeout"), stdin=subprocess.DEVNULL)
    finally:
        os.unlink(tf.name)
        shutil.rmtree(sandbox, ignore_errors=True)
    if r.returncode != 0:
        raise RuntimeError(f"opencode salió con {r.returncode}: {r.stderr.strip()[-400:]}")
    return r.stdout


def parse_json(text: str) -> dict:
    fenced = re.findall(r"```(?:json)?\s*(\{.*?\})\s*```", text, re.S)
    for cand in reversed(fenced or []):
        try:
            return json.loads(cand)
        except json.JSONDecodeError:
            pass
    a, b = text.find("{"), text.rfind("}")
    try:
        if a == -1 or b <= a:
            raise ValueError("no hay ningún objeto JSON")
        return json.loads(text[a:b + 1])
    except ValueError as e:
        # Sin la respuesta cruda, un fallo del juez no se puede diagnosticar.
        dump = state_dir() / "last-judge-output.txt"
        dump.write_text(text, encoding="utf-8")
        raise ValueError(f"el juez no devolvió JSON válido ({e}); respuesta en {rel(str(dump))}") from None


def render(template: str, **kw: str) -> str:
    text = (PROMPTS / template).read_text(encoding="utf-8")
    for k, v in kw.items():
        text = text.replace("{{" + k + "}}", v)
    return text


def grounded(f: dict, rules: dict, diffs: dict) -> bool:
    """Filtro determinista anti-alucinación: la regla tiene que existir y aplicar al fichero,
    y la evidencia citada tiene que estar de verdad en una línea añadida."""
    if f.get("rule") not in rules or f.get("file") not in diffs:
        return False
    if not rule_applies(rules[f["rule"]], f["file"]):
        return False
    norm = lambda s: re.sub(r"\s+", " ", str(s)).strip()  # noqa: E731
    ev = norm(f.get("evidence", ""))
    if len(ev) < 3:
        return False
    for ln, text in diffs[f["file"]][1]:
        line = norm(text)
        # La evidencia es parte de una línea, o abarca varias y contiene esta entera.
        if ev in line or (len(line) >= 3 and line in ev):
            f["line"] = ln
            return True
    return False


def judge(diffs: dict[str, tuple[str, list]]) -> tuple[list[dict], str]:
    """Devuelve (hallazgos, aviso). El aviso no está vacío si la revisión fue parcial."""
    rules = {r["id"]: r for r in load_rules() if r["kind"] == "semantic"
             and any(rule_applies(r, f) for f in diffs)}
    if not rules:
        return [], ""
    model, agent = cfg("judge_model"), cfg("judge_agent")
    per, max_calls = cfg_int("judge_rules_per_call"), cfg_int("judge_max_calls")

    # opencode trunca el fichero adjunto si es grande, y el juez se queda sin ver parte del
    # prompt. Así que el diff no se corta: se reparte por ficheros en paquetes de
    # `max_diff_bytes`, y cada paquete se juzga solo con las reglas de sus ficheros, en
    # tandas de `judge_rules_per_call` ("curse of instructions").
    jobs = []
    for files in pack_files(diffs):
        routed = sorted((r for r in rules.values() if any(rule_applies(r, f) for f in files)),
                        key=specificity)
        diff_txt = "\n\n".join(diffs[f][0] for f in files)
        jobs += [(rules_text(routed[i:i + per]), diff_txt) for i in range(0, len(routed), per)]
    partial = ""
    if len(jobs) > max_calls:
        partial = (f"docguard: revisión semántica parcial ({max_calls} de {len(jobs)} llamadas, "
                   f"límite judge_max_calls). El cambio es grande; conviene commitear más a menudo.")
        jobs = jobs[:max_calls]

    found: list[dict] = []
    for rules_txt, diff_txt in jobs:
        out = parse_json(run_opencode(model, render("judge.md", RULES=rules_txt, DIFF=diff_txt), agent))
        found += [f for f in out.get("violations", []) if isinstance(f, dict) and grounded(f, rules, diffs)]

    if found and cfg_bool("judge_verify"):
        # El verificador solo ve las reglas citadas y los ficheros con hallazgos.
        cited = rules_text([rules[k] for k in dict.fromkeys(f["rule"] for f in found)])
        diff_txt = "\n\n".join(diffs[f][0] for f in dict.fromkeys(f["file"] for f in found))
        out = parse_json(run_opencode(model, render(
            "verify.md", RULES=cited, DIFF=clip(diff_txt),
            FINDINGS=json.dumps(found, ensure_ascii=False, indent=1)), agent))
        keep = {int(i) for i in out.get("keep", []) if str(i).lstrip("-").isdigit()}
        found = [f for i, f in enumerate(found) if i in keep]
    return found, partial


def clip(text: str) -> str:
    """Recorta un diff a `max_diff_bytes` (solo pasa con un único fichero gigante)."""
    limit = cfg_int("max_diff_bytes")
    data = text.encode("utf-8")
    if len(data) <= limit:
        return text
    return data[:limit].decode("utf-8", "ignore") + "\n[diff truncated by max_diff_bytes]"


def pack_files(diffs: dict[str, tuple[str, list]]) -> list[list[str]]:
    """Agrupa ficheros en paquetes cuyo diff no supere `max_diff_bytes`."""
    limit, packs, cur, size = cfg_int("max_diff_bytes"), [], [], 0
    for f, (d, _) in diffs.items():
        if not d:
            continue
        n = len(d.encode("utf-8"))
        if n > limit:  # un fichero que no cabe solo: va aparte y recortado
            packs.append([f])
            diffs[f] = (clip(d), diffs[f][1])
            continue
        if cur and size + n > limit:
            packs.append(cur)
            cur, size = [], 0
        cur.append(f)
        size += n
    return packs + ([cur] if cur else [])


def rules_text(rules: list[dict]) -> str:
    return "\n".join(f"- id: {r['id']}\n  source: {r.get('source', '')}\n  applies to: "
                     f"{', '.join(r.get('paths') or ['**'])}\n  rule: {r['text']}" for r in rules)


# --- Decisiones contra los principios ------------------------------------------------

def strip_code(text: str) -> str:
    """Sin bloques de código: un `vs` o un `option` dentro de código no es una decisión."""
    return re.sub(r"```.*?```", "", text, flags=re.S)


def decide_hit(text: str, is_choice: bool) -> bool:
    """True si `text` elige entre caminos sin nombrar ningún principio. `is_choice`: el texto
    ya es una elección por definición (opciones para el usuario, un plan), no hace falta marcador."""
    text = strip_code(text)
    found = lambda key: any(re.search(m, text, re.I) for m in cfg_list(key))
    return (is_choice or found("decide_markers")) and not found("principle_markers")


def decide_gate(sid: str, text: str, is_choice: bool) -> str | None:
    """Mensaje de bloqueo, una sola vez por texto: si el agente insiste, ya lo habrá justificado."""
    if not cfg_bool("decide") or not text or not decide_hit(text, is_choice):
        return None
    h = hashlib.sha256(text.encode()).hexdigest()[:16]
    st = load_session(sid)
    if h in st.setdefault("decided", []):
        return None
    st["decided"].append(h)
    save_session(sid, st)
    return render("decide.md")


def claude_last_reply(ev: dict) -> str:
    """Texto que el agente dio en este turno: lo que va desde el último mensaje del usuario."""
    if ev.get("last_assistant_message"):
        return str(ev["last_assistant_message"])
    path, parts = ev.get("transcript_path"), []
    if not path or not Path(path).exists():
        return ""
    for line in reversed(Path(path).read_text(encoding="utf-8").splitlines()):
        try:
            e = json.loads(line)
        except ValueError:
            continue
        content = (e.get("message") or {}).get("content")
        if e.get("type") == "user":
            tool_result = isinstance(content, list) and any(
                isinstance(c, dict) and c.get("type") == "tool_result" for c in content)
            if not tool_result:
                break
        elif e.get("type") == "assistant" and isinstance(content, list):
            parts += [c.get("text", "") for c in reversed(content) if c.get("type") == "text"]
    return "\n".join(reversed(parts))


# --- Orquestación al terminar -------------------------------------------------------

def check(sid: str) -> dict:
    """Devuelve {"status": pass|fail|skip|error, "findings": [...], "message": str, "notice": str}."""
    if not cfg_bool("enabled"):
        return {"status": "skip", "findings": [], "message": "", "notice": ""}
    st = load_session(sid)
    files = changed_since(st)
    if not files:
        notice = ""
        # Solo sigue pendiente lo que está igual que cuando se revisó. Si el agente lo
        # arregló volviendo al contenido original, el fichero ya no sale como cambiado.
        now = hash_files([p["file"] for p in st.get("pending", [])])
        unresolved = [p for p in st.get("pending", [])
                      if now.get(p["file"], DELETED) == st["reviewed"].get(p["file"])]
        if unresolved:
            notice = format_pending(unresolved)
        # Lo resuelto se olvida como revisado: si el agente vuelve a ese mismo estado roto,
        # hay que volver a bloquearlo, no darlo por pagado.
        for p in st.get("pending", []):
            if p not in unresolved:
                st["reviewed"].pop(p["file"], None)
        if st.get("pending"):
            st["pending"], st["rounds"] = [], 0
            save_session(sid, st)
        return {"status": "skip", "findings": [], "message": "", "notice": notice}

    diffs = {f: file_diff(f, st) for f in files}
    hashes = hash_files(files)
    # De barato a caro: si falla una verificación determinista, ni se paga el juez.
    findings, slow = run_checks(files)
    findings = findings or forbid_findings(diffs)
    error, partial = "", slow
    if not findings:
        try:
            findings, judged = judge(diffs)
            partial = "\n".join(x for x in (slow, judged) if x)
        except Exception as e:  # infraestructura: nunca bloquea el trabajo
            error = f"docguard: el juez falló y no se revisó la parte semántica ({e})."

    # Lo revisado en este estado no se vuelve a pagar.
    for f in files:
        st["reviewed"][f] = hashes.get(f, "deleted")

    seen: set = set()  # una línea, un aviso: varias reglas sobre lo mismo solo hacen ruido
    findings = [f for f in findings if (f["file"], f.get("line")) not in seen
                and not seen.add((f["file"], f.get("line")))][:cfg_int("max_findings")]
    # Hallazgos anteriores en ficheros que el agente no volvió a tocar: no se re-juzgan
    # (ya se pagaron), pero tampoco se pierden: llegan al usuario si el agente para.
    carry = [p for p in st.get("pending", []) if p["file"] not in files]
    if not findings:
        st["rounds"], st["pending"] = 0, []
        save_session(sid, st)
        notice = "\n\n".join(x for x in (error, partial, format_pending(carry) if carry else "") if x)
        return {"status": "error" if error else "pass", "findings": [], "message": "", "notice": notice}

    st["rounds"] += 1
    if st["rounds"] > cfg_int("max_rounds"):
        st["rounds"], st["pending"] = 0, []
        save_session(sid, st)
        return {"status": "pass", "findings": findings, "message": "",
                "notice": "docguard: se agotaron las rondas de corrección. Pendiente:\n"
                          + format_findings(findings + carry)}
    st["pending"] = findings + carry
    save_session(sid, st)
    message = format_block(findings) + (f"\n\n{partial}" if partial else "")
    return {"status": "fail", "findings": findings, "message": message, "notice": partial}


def format_findings(findings: list[dict]) -> str:
    rules = {r["id"]: r for r in load_rules()}
    out = []
    for i, f in enumerate(findings, 1):
        if f.get("command"):
            out.append(f"{i}. [CHECK] `{f['command']}` falló (cambios en {f['file']}). Salida:\n"
                       + "\n".join("   " + x for x in str(f["evidence"]).splitlines()))
            continue
        r = rules.get(f["rule"], {})
        out.append(f"{i}. [{f['rule']}] {r.get('source', '')}\n"
                   f"   Regla: «{r.get('text', '')}»\n"
                   f"   {f['file']}:{f.get('line', '?')} — `{str(f.get('evidence', ''))[:160]}`"
                   + (f"\n   Problema: {f['problem']}" if f.get("problem") else "")
                   + (f"\n   Arreglo: {f['fix']}" if f.get("fix") else ""))
    return "\n".join(out)


def format_block(findings: list[dict]) -> str:
    return ("docguard: lo que has cambiado no cumple estas reglas de la documentación del repo.\n"
            "Corrige cada punto. Si alguno es un falso positivo, justifícalo en una línea en tu "
            "respuesta y no lo toques.\n\n" + format_findings(findings))


def format_pending(findings: list[dict]) -> str:
    return ("docguard: el agente terminó sin cambiar estos puntos (los habrá justificado o "
            "ignorado):\n" + format_findings(findings))


# --- Adaptadores --------------------------------------------------------------------

EDIT_KEYS = ("file_path", "filePath", "notebook_path", "path")
READ_TOOLS = {"read", "glob", "grep", "list", "ls"}


def tool_target(tool: str, args: dict) -> tuple[str, str | None, str | None]:
    """Normaliza (tipo, comando, ruta) entre Claude Code y opencode."""
    t = tool.lower()
    if t == "bash":
        return "bash", args.get("command"), None
    for k in EDIT_KEYS:
        if isinstance(args.get(k), str):
            return ("read" if t in READ_TOOLS else "edit"), None, args[k]
    return t, None, None


def hook_claude(ev: dict) -> dict:
    name, sid = ev.get("hook_event_name"), ev.get("session_id", "claude")
    if not cfg_bool("enabled"):
        return {}
    if name == "SessionStart":
        load_session(sid)
        return {}
    if name == "PreToolUse":
        tool, args = ev.get("tool_name", ""), ev.get("tool_input") or {}
        if tool in cfg_list("decide_tools"):
            gate = decide_gate(sid, json.dumps(args, ensure_ascii=False), is_choice=True)
            if gate:
                return {"hookSpecificOutput": {"hookEventName": "PreToolUse",
                                               "permissionDecision": "deny",
                                               "permissionDecisionReason": gate}}
        kind, cmd, path = tool_target(tool, args)
        r = guard(kind, cmd, path)
        if r:
            return {"hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "ask" if r.get("action") == "ask" else "deny",
                "permissionDecisionReason": deny_message(r)}}
        loop = None
        if kind == "edit" and path:
            touch(sid, path)
            loop = loop_notice(sid, path)
        ctx = join_ctx(jit(sid, path) if path else None, loop)
        if ctx:
            return {"hookSpecificOutput": {"hookEventName": "PreToolUse", "additionalContext": ctx}}
        return {}
    if name == "Stop":
        res = check(sid)
        if res["status"] == "fail":
            return {"decision": "block", "reason": res["message"]}
        gate = decide_gate(sid, claude_last_reply(ev), is_choice=False)
        if gate:
            return {"decision": "block", "reason": gate}
        return {"systemMessage": res["notice"]} if res["notice"] else {}
    return {}


def hook_opencode(ev: dict) -> dict:
    """Contrato con `.opencode/plugins/docguard.ts`: devuelve {block, context, prompt, notice}."""
    kind_ev, sid = ev.get("event"), ev.get("session_id", "opencode")
    out: dict[str, str | None] = {"block": None, "context": None, "prompt": None, "notice": None}
    if not cfg_bool("enabled"):
        return out
    if kind_ev == "session_created":
        load_session(sid)
    elif kind_ev == "pre_tool":
        tool, args = ev.get("tool", ""), ev.get("args") or {}
        if tool in cfg_list("decide_tools"):
            out["block"] = decide_gate(sid, json.dumps(args, ensure_ascii=False), is_choice=True)
            if out["block"]:
                return out
        kind, cmd, path = tool_target(tool, args)
        r = guard(kind, cmd, path)
        if r:
            out["block"] = deny_message(r)
    elif kind_ev == "post_tool":
        kind, _, path = tool_target(ev.get("tool", ""), ev.get("args") or {})
        loop = None
        if kind == "edit" and path:
            touch(sid, path)
            loop = loop_notice(sid, path)
        out["context"] = join_ctx(jit(sid, path) if path else None, loop)
    elif kind_ev == "idle":
        res = check(sid)
        out["prompt"] = res["message"] if res["status"] == "fail" else None
        if not out["prompt"]:
            out["prompt"] = decide_gate(sid, ev.get("text") or "", is_choice=False)
        out["notice"] = res["notice"] or None
    return out


# --- Compilación de reglas ----------------------------------------------------------

def sections(path: str, level: int) -> list[tuple[str, str]]:
    text = (ROOT / path).read_text(encoding="utf-8")
    marker = "#" * level + " "
    out, title, buf, fence = [], "(inicio)", [], False
    for line in text.splitlines():
        if line.startswith("```"):
            fence = not fence
        if not fence and line.startswith(marker):
            out.append((title, "\n".join(buf)))
            title, buf = line[len(marker):].strip(), []
        buf.append(line)
    out.append((title, "\n".join(buf)))
    return [(t, b) for t, b in out if b.strip()]


def compile_rules(force: bool) -> int:
    data = json.loads(RULES_FILE.read_text(encoding="utf-8")) if RULES_FILE.exists() else {}
    rules = data.get("rules", [])
    manual = [r for r in rules if r.get("origin") != "compiled"]
    old = [r for r in rules if r.get("origin") == "compiled"]
    seen: dict[str, str] = data.get("sections", {})  # sección -> hash con el que se compiló

    level, keep, fresh, sections_now, calls = cfg_int("compile_heading_level"), [], [], {}, 0
    for src in cfg_list("compile_sources"):
        if not (ROOT / src).exists():
            print(f"⚠ {src} no existe, se salta", file=sys.stderr)
            continue
        todo = []
        for title, body in sections(src, level):
            key = f"{src}#{title}"
            sections_now[key] = hashlib.sha256(body.encode()).hexdigest()[:16]
            if force or seen.get(key) != sections_now[key]:
                todo.append((title, body, key))
        keep += [r for r in old if r["section_key"].startswith(src + "#")
                 and r["section_key"] not in {k for _, _, k in todo}]
        if not todo:
            print(f"✔ {src}: sin cambios", file=sys.stderr)
            continue
        print(f"… {src}: {len(todo)} sección(es) a compilar", file=sys.stderr)
        doc = "\n\n".join(f"<section id=\"{i}\" title=\"{t}\">\n{b}\n</section>"
                          for i, (t, b, _) in enumerate(todo))
        out = parse_json(run_opencode(cfg("compile_model"),
                                      render("compile.md", SOURCE=src, SECTIONS=doc,
                                             EXISTING=rules_text(manual) or "(none)"), cfg("judge_agent")))
        calls += 1
        for r in out.get("rules", []):
            try:
                title, _, key = todo[int(r["section"])]
                if r["kind"] not in KINDS or not r.get("text"):
                    raise ValueError("kind o text inválidos")
                if r["kind"] in ("deny", "forbid"):
                    re.compile(r["pattern"])
            except (KeyError, ValueError, IndexError, re.error) as e:
                print(f"⚠ regla descartada ({e}): {r}", file=sys.stderr)
                continue
            rule = {"id": Path(src).stem.split("-")[0].upper() + "-"
                          + hashlib.sha256((key + r["text"]).encode()).hexdigest()[:6],
                    "kind": r["kind"], "text": r["text"].strip(), "source": f"{src} §{title}",
                    "paths": r.get("paths") or ["**"], "origin": "compiled", "section_key": key}
            for k in ("pattern", "tool", "action", "exclude", "hint"):
                if r.get(k):
                    rule[k] = r[k]
            # Una `deny` bloquea acciones sin pasar por ningún juicio: nace propuesta y la
            # activa un humano ("enabled": true), igual que un 🔶 PROPUESTO del repo.
            if rule["kind"] == "deny":
                rule["enabled"] = False
            fresh.append(rule)

    compiled = keep + fresh
    if calls:
        compiled, merged = dedupe(manual, compiled)
        calls += 1
        print(f"✔ deduplicación: {merged} regla(s) repetida(s) fusionada(s)", file=sys.stderr)
    data = {"_doc": data.get("_doc", ""), "rules": manual + sorted(compiled, key=lambda r: (r["section_key"], r["id"])),
            "sections": sections_now}
    RULES_FILE.write_text(json.dumps(data, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    proposed = sum(1 for r in compiled if r.get("enabled") is False)
    print(f"✔ {len(compiled)} reglas compiladas, {proposed} propuestas sin activar "
          f"({calls} llamada(s) al modelo) → {rel(str(RULES_FILE))}", file=sys.stderr)
    return 0


def dedupe(manual: list[dict], compiled: list[dict]) -> tuple[list[dict], int]:
    """Una llamada: agrupa reglas que dicen lo mismo. Gana la manual; si no hay, la primera.
    Las rutas de las descartadas se suman a la que queda, para no perder alcance."""
    pool = manual + compiled
    listing = "\n".join(f"- {r['id']} [{r['kind']}] ({', '.join(r.get('paths') or ['**'])}): {r['text']}"
                         for r in pool)
    try:
        groups = parse_json(run_opencode(cfg("compile_model"), render("dedupe.md", RULES=listing),
                                         cfg("judge_agent"))).get("duplicates", [])
    except Exception as e:  # sin deduplicar sigue siendo correcto, solo más largo
        print(f"⚠ deduplicación omitida ({e})", file=sys.stderr)
        return compiled, 0
    by_id = {r["id"]: r for r in pool}
    drop: set[str] = set()
    for g in groups:
        ids = [i for i in g if i in by_id and i not in drop]
        if len(ids) < 2:
            continue
        # Prioridad: manual > forbid (determinista, gratis) > la mejor redactada.
        winner = next((i for i in ids if by_id[i].get("origin") != "compiled"),
                      next((i for i in ids if by_id[i]["kind"] == "forbid"), ids[0]))
        for i in ids:
            if i == winner or by_id[i].get("origin") != "compiled":
                continue
            w = by_id[winner]
            if w.get("origin") == "compiled":
                w["paths"] = sorted(set(w.get("paths") or ["**"]) | set(by_id[i].get("paths") or ["**"]))
                if "**" in w["paths"]:
                    w["paths"] = ["**"]
            drop.add(i)
    return [r for r in compiled if r["id"] not in drop], len(drop)


# --- Eval ---------------------------------------------------------------------------

def run_eval(use_judge: bool) -> int:
    """Gold set: cada caso es un JSON {file, before, after, expect: [ids de regla], note}.
    `expect` vacío = cambio limpio (mide falsos positivos). Se puntúa por pares (caso, regla)."""
    cases_dir = HERE / cfg("eval_dir")
    cases = sorted(cases_dir.glob("*.json"))
    if not cases:
        print(f"no hay casos en {rel(str(cases_dir))}", file=sys.stderr)
        return 2
    det = {r["id"] for r in load_rules() if r["kind"] == "forbid"}
    tp = fp = fn = 0
    report = []
    for path in cases:
        c = json.loads(path.read_text(encoding="utf-8"))
        diffs = {c["file"]: make_diff(c["file"], c.get("before", "").splitlines(),
                                      c["after"].splitlines())}
        error = ""
        got = {f["rule"] for f in forbid_findings(diffs)}
        if not got and use_judge:
            try:
                got = {f["rule"] for f in judge(diffs)[0]}
            except Exception as e:
                error = str(e)
        want = set(c.get("expect", []))
        if not use_judge:  # sin juez solo se puntúan las reglas deterministas
            want &= det
        ok, extra, miss = got & want, got - want, want - got
        tp, fp, fn = tp + len(ok), fp + len(extra), fn + len(miss)
        mark = "✔" if not extra and not miss and not error else "✘"
        print(f"{mark} {path.stem}: esperado {sorted(want) or '—'} · obtenido {sorted(got) or '—'}"
              + (f" · ERROR {error}" if error else ""))
        report.append({"case": path.stem, "want": sorted(want), "got": sorted(got), "error": error})
    prec = tp / (tp + fp) if tp + fp else 1.0
    recall = tp / (tp + fn) if tp + fn else 1.0
    print(f"\nprecisión {prec:.2f} · recall {recall:.2f} · {len(cases)} casos "
          f"(tp {tp}, fp {fp}, fn {fn}){'' if use_judge else ' · sin juez'}")
    out = state_dir() / "last-eval.json"
    out.write_text(json.dumps({"precision": prec, "recall": recall, "judge": use_judge,
                               "model": cfg("judge_model"), "cases": report},
                              ensure_ascii=False, indent=1), encoding="utf-8")
    print(f"informe → {rel(str(out))}")
    return 0 if not fp and not fn else 1


# --- CLI ----------------------------------------------------------------------------

def arg(flag: str, default: str | None = None) -> str | None:
    a = sys.argv
    return a[a.index(flag) + 1] if flag in a and a.index(flag) + 1 < len(a) else default


def main() -> int:
    cmd = sys.argv[1] if len(sys.argv) > 1 else "help"
    if cmd == "hook":
        try:
            ev = json.loads(sys.stdin.read() or "{}")
            res = hook_claude(ev) if sys.argv[2] == "claude" else hook_opencode(ev)
        except Exception as e:  # un hook roto nunca debe tumbar la sesión del agente
            res = {"systemMessage": f"docguard: error interno ({e})"} if sys.argv[2] == "claude" \
                else {"block": None, "context": None, "prompt": None, "notice": f"docguard: error interno ({e})"}
        print(json.dumps(res, ensure_ascii=False))
        return 0
    if cmd == "check":
        sid = arg("--session") or "manual"
        if sid == "manual" and "--session" not in sys.argv:
            session_path(sid).unlink(missing_ok=True)  # manual: revisa todo lo que hay sin commitear
            st = load_session(sid)
            st["baseline"], st["manual"] = {}, True
            save_session(sid, st)
        res = check(sid)
        if res["status"] == "fail":
            print(format_findings(res["findings"]))
            if res["notice"]:
                print("\n" + res["notice"])
        elif res["notice"]:
            print(res["notice"])
        else:
            print("✔ docguard: nada que objetar" if res["status"] == "pass" else "· docguard: no hay cambios que revisar")
        if sid == "manual" and "--session" not in sys.argv:
            session_path(sid).unlink(missing_ok=True)
        return 1 if res["status"] == "fail" else 0
    if cmd == "rules":
        target = sys.argv[2] if len(sys.argv) > 2 else None
        for r in load_rules():
            if target and not rule_applies(r, rel(target)) and r["kind"] != "deny":
                continue
            print(f"[{r['kind']:8}] {r['id']:14} {r['text']}\n{'':26}{r.get('source', '')}")
        return 0
    if cmd == "decide":
        hit = decide_hit(sys.stdin.read(), is_choice="--tool" in sys.argv)
        print(render("decide.md") if hit else "✔ docguard: decide no pararía este texto")
        return 1 if hit else 0
    if cmd == "eval":
        return run_eval("--no-judge" not in sys.argv)
    if cmd == "compile":
        return compile_rules("--force" in sys.argv)
    if cmd == "reset":
        sid = arg("--session")
        targets = [session_path(sid)] if sid else list(state_dir().glob("*.json"))
        for p in targets:
            p.unlink(missing_ok=True)
        print(f"✔ {len(targets)} sesión(es) olvidada(s)")
        return 0
    print(__doc__)
    return 0 if cmd in ("help", "-h", "--help") else 2


if __name__ == "__main__":
    sys.exit(main())
