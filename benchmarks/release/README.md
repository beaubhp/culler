# Cull Release Benchmark

This directory contains the reproducible release benchmark. The benchmark has two layers:

- `fixtures/` plus `labels/`: constructed projects with full definition-level truth labels.
- `real_repositories` in `suite.json`: pinned external repositories used as trust checks. These
  are manually reviewed around tool findings and high-confidence Cull findings; they are not a full
  recall ground truth.

The release gate scores only the current reportable unit: top-level Python functions and classes.
Methods, nested functions, modules, and dependency findings are outside the current scoring surface.

## Commands

Build Cull first:

```bash
cargo build --release
```

Validate benchmark metadata and labels:

```bash
python3 benchmarks/release/run.py --validate-only
python3 benchmarks/release/test_run.py
```

Run the constructed truth corpus with baselines:

```bash
python3 benchmarks/release/run.py \
  --cull target/release/cull \
  --results results/latest.json
```

Run the constructed corpus plus pinned real-repository trust checks:

```bash
python3 benchmarks/release/run.py \
  --cull target/release/cull \
  --include-real \
  --real-root .context/benchmarks/release/real \
  --results results/latest-with-real.json
```

The real-repository checkouts live under `.context/` and are intentionally not tracked.

## Baselines

Baseline commands and versions are pinned in `suite.json`:

- Vulture 2.16: `uvx vulture==2.16 <source-roots>`
- deadcode 2.4.1: `uvx --python 3.11 deadcode==2.4.1 <source-roots>`

Baseline scoring filters to top-level function/class findings that match labeled definitions.
Unmatched nested or method findings are recorded as tool output volume but not scored against the
current truth surface. Cull unmatched findings are a release-gate failure because they indicate
label drift, schema drift, or an unmodeled reportable surface.

`--skip-baselines` is only a local smoke option. The release gate requires baseline metrics and will
not pass without them.

## Labels

Label files use:

- `dead`: expected dead top-level definition.
- `live`: expected live top-level definition; a report here is a false positive.
- `unknown`, `test_only`, `out_of_scope`: excluded from precision/recall scoring but counted as
  ignored findings.
- `expected_rule`: required on `dead` labels and enforced for Cull scoring.
- `expected_confidence` or `max_confidence`: optional confidence assertions enforced for Cull
  scoring.

The constructed corpus includes semantic fixtures, application and library modes, partial-analysis
cases, and a constructed holdout. Real repositories add pinned application/library trust checks
without pretending to be exhaustive recall labels.

## Gate

Thresholds are in `suite.json`. The gate requires:

- nontrivial labeled dead definitions and Cull true positives
- application, auto, and library truth-corpus reporting
- baseline metrics
- zero high-confidence false positives
- zero unmatched Cull findings
- zero high-confidence ignored-label findings
- zero Cull rule or confidence mismatches
- high-confidence precision at or above the pinned floor
- reported recall at or above the pinned floor
- holdout labeled-dead, precision, false-positive, and recall checks
- high-confidence precision above the best available baseline precision
- per-case and full-corpus runtime within budget
- peak memory within budget

JSON result files contain raw command records, output hashes, metrics, review/suppressed counts,
runtime, and memory. Markdown summaries are generated beside the JSON files for quick review.
Generated result files are local artifacts and are ignored by git.
