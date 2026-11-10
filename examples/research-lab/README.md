# Research lab fixtures

A tiny corpus (`corpus/rust.md`, `corpus/python.md`) and a JSONL dataset
(`questions.jsonl`, one `{"query", "expect_source_ids"}` per line) for the `rag-lab`
template. No model, no network, no key.

```sh
# 1. A core with this repository's templates (any home works; this one is disposable).
export REGULUS_HOME=$(mktemp -d); mkdir $REGULUS_HOME/profiles
printf '[server]\nport = 0\n\n[templates]\npaths = ["%s/templates"]\n' "$PWD" > $REGULUS_HOME/config.toml
regulus serve --port 0 &

# 2. Apply the lab to the corpus and run it. Paths are absolute.
regulus template apply rag-lab \
  --set docs_dir=$PWD/examples/research-lab/corpus \
  --set dataset=$PWD/examples/research-lab/questions.jsonl
regulus run rag-lab --detach                  # exec-1  lab  running
regulus send exec-1 go now                    # prints the event trace of the whole run

# 3. Read the result (the three outputs that matter).
regulus call event.list '{"execution_id":"exec-1","limit":10000}' |
  python3 -c 'import json,sys; [print(e["from"]["port_id"], json.dumps(e["value"])[:300]) for e in json.load(sys.stdin)["events"] if e["from"]["node_id"]=="self" and e["kind"]=="emitted" and e["from"]["port_id"] in ("dataset","candidates","comparison","winner")]'
regulus call data.export '{"execution_id":"exec-1","node_id":"collect"}'   # the collected dataset
regulus stop exec-1
```

What happens: `docs_dir` is ingested twice, into the collections `rag-lab-a` and `rag-lab-b`
(candidate `a`: markdown chunks of 1200 characters, `b`: fixed chunks of 300); the dataset is
read, deduplicated by the whole case (an identical line folds; the same query with other
expected ids is another case) and bounded by the `data` module's `collect` node; every query
runs through the workflow of each candidate (`retrieve_a`, `retrieve_b`); `documents`'
`recall` scores each (the same recall@k as `rag-eval`) and `rank` compares them, only when
both were scored on the same, non-zero number of items (otherwise `self.error` says why).
`self.comparison` holds the ranking, `self.candidates` the metrics of each candidate with
their per-item hits, `self.winner` the best label (both, joined, on a tie).

**Honest result on these fixtures:** the two documents are one chunk each and every query
finds its file at `k=1`, so both candidates reach recall 1.0 and the lab reports a tie
(`tie: true`). That is the correct answer for this corpus, not a bug. To see candidates
differ, make one worse or the corpus harder:

```sh
regulus template apply rag-lab --id lab-k1 \
  --set docs_dir=$PWD/examples/research-lab/corpus \
  --set dataset=$PWD/examples/research-lab/questions.jsonl \
  --set k_a=1 --set k_b=3 --set label_a=top1 --set label_b=top3
```

(`crates/regulus-cli/tests/template_rag_lab.rs` builds a three-file corpus where `k_a=1`
scores 2/3 and `k_b=3` scores 3/3.) Every variable is declared under `[template.vars]` in `regulus template show rag-lab`. Your own
candidate is a workflow with the input `in` and the outputs `hits` and `error` (see the
header of `templates/rag-lab.toml`); point `candidate_a=<id>` at it.

Re-runs: each candidate's collection name carries its chunker, size and overlap, so changing
`chunk_chars_a` and so on ingests afresh. Files deleted from the corpus stay in their old
collections (use another `--set collection=…` or `regulus docs rm`), and wiring another
retrieval engine also calls for a new `collection`.

Limits: two candidates are drawn (a third means copying `ingest_b`, `batch_b`, `recall_b`
and a `pair` input); ingestion of both collections finishes before the first query because
its path through the bus is shorter, not because anything waits for it; a retrieval engine
module is wired into `ingest_a.engine` and `batch_a.engine` but no external engine is
shipped or exercised by the tests.
