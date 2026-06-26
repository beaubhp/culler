# Culler

Culler is a fast, precise dead-code analyzer for Python projects.

It is built in Rust, distributed as a single command-line tool, and designed to
find dead Python code without turning ordinary public APIs, exports, tests, or
dynamic edges into noisy findings.

```bash
culler check .
```

## Why Culler

Dead-code detection is easy to make loud and hard to make useful. Culler is
intentionally conservative where Python is dynamic, and specific where static
evidence is strong.

Culler currently reports:

- unreferenced functions and classes;
- production code unreachable from known application roots;
- unused imports;
- unused local bindings;
- unreachable statement ranges;
- unused private methods.

Findings are split by confidence. High-confidence findings are shown by default.
Review findings remain available when useful, but they do not fail a default
text run.

## Installation

The recommended installation method for the CLI is `pipx` or `uv tool`.

```bash
pipx install culler
```

```bash
uv tool install culler
```

You can also install it into an existing Python environment:

```bash
python -m pip install culler
```

The PyPI package installs the compiled `culler` binary. It does not expose a
Python import API yet.

## Quick Start

Run Culler on the current project:

```bash
culler check .
```

Emit machine-readable JSON:

```bash
culler check . --format json
```

Show review-confidence findings as well as high-confidence findings:

```bash
culler check . --show-review
```

Explain a specific candidate or finding:

```bash
culler explain <candidate-or-finding-id> .
```

Exit codes are intentionally small:

| Code | Meaning |
| --- | --- |
| `0` | Analysis completed without default-visible findings. |
| `1` | Analysis completed and default-visible findings were reported. |
| `2` | Input, configuration, parse, or completeness error. |

## Configuration

Culler reads configuration from `pyproject.toml`.

```toml
[tool.culler]
src = "src"
mode = "auto"
target-python = "3.12"
```

For applications, declare production roots when you want reachability findings:

```toml
[tool.culler]
src = "src"
mode = "application"
root_coverage = "complete"
roots = ["my_app.cli:main"]
```

For libraries, Culler treats exported and externally visible surfaces more
conservatively:

```toml
[tool.culler]
src = "src"
mode = "library"
```

Useful fields:

| Field | Purpose |
| --- | --- |
| `src` | Source root or roots to analyze. |
| `mode` | `auto`, `application`, or `library`. |
| `root_coverage` | `partial` or `complete` when roots are known. |
| `roots` | Application entry points such as `pkg.cli:main`. |
| `tests` | Test paths when they are not discoverable conventionally. |
| `target-python` | Python syntax/semantic target, currently `3.10` through `3.15`. |
| `exclude` | Glob patterns to exclude from analysis. |
| `allow_partial` | Permit partial analysis without escalating to exit code `2`. |

## Output

Text output is optimized for local development and CI logs. JSON output is
intended for editors, dashboards, benchmarks, and automation.

```bash
culler check . --format json
```

Rule IDs are stable machine-readable identifiers:

| Rule | Finding |
| --- | --- |
| `CULL001` | Unreferenced function. |
| `CULL002` | Unreferenced class. |
| `CULL003` | Root-unreachable function. |
| `CULL004` | Root-unreachable class. |
| `CULL005` | Unused import binding. |
| `CULL006` | Unused local binding. |
| `CULL007` | Unreachable statement range. |
| `CULL008` | Unused private method. |

Diagnostic IDs such as `CULL_P0101` describe analysis, parsing, or
configuration problems.

## Benchmark

Culler includes a fixed benchmark corpus under [`benchmark/`](benchmark/). The
corpus is artificial but intentionally realistic: multi-module services,
libraries, CLIs, workers, pipelines, plugin systems, configuration packages, and
utility-heavy AI-era projects.

The benchmark compares Culler with Vulture and deadcode across precision,
recall, F1, runtime, and peak RSS where available. Results should be read as
evidence for this corpus, not as a universal claim about every Python project.

Run the benchmark locally:

```bash
cargo build --release
python3 benchmark/run.py --culler target/release/culler --tools culler,vulture,deadcode
```

See [`benchmark/README.md`](benchmark/README.md) for methodology and scoring.

## Development

Prerequisites:

- Rust 1.82 or newer;
- Python 3.10 or newer;
- `uv` for packaging and benchmark helper commands.

Core checks:

```bash
cargo fmt --all --check
cargo check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
python3 benchmark/test_run.py
python3 benchmark/run.py --validate-only
```

Build the PyPI package locally:

```bash
uvx maturin build --release --out dist --sdist
uvx twine check dist/*
```

## Release Status

Culler is pre-1.0 software. The rule IDs are intended to be stable, but CLI,
configuration, and JSON output details may still evolve before `1.0`.

Releases use reviewed release PRs and a changelog. See
[`CONTRIBUTING.md`](CONTRIBUTING.md) for commit discipline and release notes.

## License

Culler is released under the MIT License. See [`LICENSE`](LICENSE).
