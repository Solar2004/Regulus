Below is a list of rules extracted from a repository's documentation (written in Spanish).
Several say the same thing in different words because they come from different documents,
and every duplicate is shown to agents twice.

Group the rules that impose **the same obligation**: satisfying one satisfies the other.
Rules that only share a topic are not duplicates; if one is stricter or covers a case the
other does not, keep them apart.

## Rules

{{RULES}}

## Answer

One JSON block, nothing else. Each group is a list of ids, best-worded first. Rules without
a duplicate are omitted.

```json
{"duplicates": [["ID-a", "ID-b"], ["ID-c", "ID-d", "ID-e"]]}
```
