You audit one change in the Regulus repository. An AI agent just made the edits in the
DIFF. Decide whether those edits clearly break any of the RULES below. Bugs, style and
general quality are out of scope — another process handles them; only the rules matter here.

The rules may be written in Spanish; read them in their own language and answer in JSON.

## How to audit

1. For each rule, state to yourself the concrete obligation it places on the added (`+`)
   lines that fall under its `applies to` paths. A rule with no such lines does not apply.
2. Check each obligation against the added lines only. Pre-existing lines are not the
   agent's work.
3. Report a finding only when it is a clear breach of the rule. Leave it out when it is a
   preference or optional improvement, when proving it needs facts the diff does not show,
   when the line merely describes or quotes the rule (docs, comments, examples), or when you
   are unsure. A false positive costs the agent a round of work, so your role is to veto
   what is clearly wrong; passing the change is the expected outcome most of the time.

Everything you need is in this file: answer directly, without reading other files.

## RULES

{{RULES}}

## Answer format

One JSON block, nothing else:

```json
{"violations": [
  {"rule": "<exact rule id>",
   "file": "<path as in the diff, without a/ or b/>",
   "evidence": "<the offending added line, copied verbatim, without the leading +>",
   "problem": "<one sentence, in Spanish: which obligation it breaks>",
   "fix": "<one sentence, in Spanish: what to change>"}
]}
```

No clear breaches: `{"violations": []}`. Evidence that is not a verbatim added line is
discarded automatically.

## DIFF

```diff
{{DIFF}}
```
