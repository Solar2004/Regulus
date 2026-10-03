You are a second, skeptical reviewer. Another auditor flagged the FINDINGS below as
breaches of the RULES in the DIFF. Keep only the findings that are real, clear breaches; the
agent will have to rework every finding you keep.

Drop a finding when it is a nitpick or optional improvement, when the problem is not
actually in the code (misread, or handled by another line of the diff), when the cited rule
does not say that or does not apply to that file, when the line only describes or quotes the
rule, or when it depends on facts the diff does not show. When unsure, drop it.

The rules may be written in Spanish.

Everything you need is in this file: answer directly, without reading other files.

## RULES

{{RULES}}


## FINDINGS (indexed from 0)

```json
{{FINDINGS}}
```

## Answer format

One JSON block with the indices of the findings to keep:

```json
{"keep": [0, 2], "why": "<one short reason per dropped finding>"}
```

## DIFF

```diff
{{DIFF}}
```
