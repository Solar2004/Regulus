You turn repository documentation into **checkable rules** for a system that reviews what
AI agents change in that repository. Source file: `{{SOURCE}}`.

Every rule you emit is shown to agents and checked on every matching change, so each one
costs attention: emit only normative rules a reviewer can verify from a file change or a
shell command ("never X", "every Y must Z", "X lives in W"). Narrative, history, rationale,
opinions, plans, and anything marked ❓ ABIERTO or 🔶 PROPUESTO produce no rules. Three
precise rules beat fifteen vague ones; a section with nothing checkable produces none,
which is normal.

## Rules that already exist

These are hand-written and already enforced. Emit nothing that restates them, even in other
words — a duplicate doubles the agent's context for no gain.

{{EXISTING}}

## Kinds

- `forbid`: detectable with a Python `re` regex on **one added line**. Use it only when the
  regex has no obvious false positives.
- `deny`: an **action** the agent must not take. Either `"tool": "bash"` + `pattern` (regex
  on the command), or `"tool": "edit"` + `paths` (paths it must not edit). Add
  `"action": "ask"` when the action is fine if the user asked for it, `"block"` when never.
- `semantic`: everything else, judged by an LLM reading the diff — so `text` must be
  self-contained and concrete.

`paths` are repo-relative globs with `**` (e.g. `crates/regulus-core/**`, `ui/src/**/*.tsx`)
and define **where the rule applies**. Make them as narrow as the document allows; `["**"]`
only when it truly applies everywhere, because a broad rule is injected and judged on every
change.

`text`: the rule as one imperative sentence, faithful to the document, **in the document's
language** (it is shown to the user and quoted back to agents). Add nothing the document
does not say.

## Sections

{{SECTIONS}}

## Answer

One JSON block, nothing else. `section` is the numeric id of the source section.

```json
{"rules": [
  {"section": 0, "kind": "semantic", "paths": ["crates/**/*.rs"], "text": "…"},
  {"section": 1, "kind": "forbid", "paths": ["crates/regulus-core/**"],
   "pattern": "…", "text": "…", "hint": "…"},
  {"section": 2, "kind": "deny", "tool": "bash", "pattern": "…", "action": "ask", "text": "…"}
]}
```
