# eval/ — retrieval-quality harness

A labeled `query → relevant-doc` evaluation set and a runner that reports **nDCG@10, recall@k, and MRR** (spec §7, §18). Used to gate merges: any change that regresses retrieval quality below the recorded baseline fails CI.

## What it gates
- Embedding model / backend swaps (e.g., Rust-native `candle` ↔ Python BGE-M3; ADR-0004 / ADR-0009).
- Chunking parameter changes (size/overlap/structure rules).
- Reranker changes and fusion-weight (RRF vs weighted) tuning.
- Context-condense on/off.

## Layout (planned)
```
eval/
├─ datasets/            # labeled query sets (per corpus); ground-truth relevance judgments
├─ runner/              # Rust or Python harness that runs queries through retrieval and scores them
├─ baselines/           # recorded metric baselines per dataset+config (the regression gate)
└─ reports/             # generated nDCG@10 / recall@k / MRR reports (gitignored or artifacted)
```

## Run (planned)
```bash
just eval                      # run all datasets, compare to baselines, fail on regression
just eval-baseline <dataset>   # record a new baseline (reviewed change only)
```

## Notes
- Leaderboard scores (e.g., MTEB) are a **starting point only** — always evaluate on the user's own corpus.
- The harness reads retrieval results via the same `retrieval` service path used in production, so eval reflects real fusion + rerank + `ACTIVE`-filter behavior.

> Scaffolding placeholder — stood up alongside phase **M2** and enforced from **M2** onward (spec §20).
