# Part 0 Implementation Note

Document type: implementation note
Status: Part 0 decisions recorded
Audience: implementation and review
Last updated: 2026-06-23

## Summary

Part 0 establishes the Rust workspace, deterministic Python project discovery, canonical source
decoding, parser-neutral Cull data structures, Ruff-backed parsing, and the hidden debug
definitions command.

```bash
cull debug definitions path/to/project --format json
```

The command emits versioned JSON containing source roots, modules, top-level function and class
definitions, canonical UTF-8 byte ranges, and structured diagnostics. It does not emit findings.

## Parser Decision

Cull v0 uses Ruff's parser crates through a pinned git dependency:

```text
https://github.com/astral-sh/ruff
rev 7033119ac2a7fb82e553afec621dd6f72f4f4720
```

Ruff viability gate:

| Gate | Result |
|---|---|
| Dependency or vendoring mechanism | Passed as a pinned git dependency. |
| Required API access | Passed through `ruff_python_parser`, `ruff_python_ast`, and `ruff_text_size`. |
| Rust toolchain policy | Passed with Rust 1.96.0; Ruff requires Rust 1.94 on current main. |
| Compile footprint | Initial probe `cargo check` completed in about 22 seconds locally. |
| License and pinning | Ruff is MIT licensed; dependency is pinned by revision. |

`rustpython-parser` 0.4.0 remains the comparison baseline but is not retained in source. It parsed
the core 3.10-3.12-style corpus but failed the template-string canary:

```text
FAIL template_string: invalid syntax. Got unexpected token "hello {name}" at byte offset 12
```

Ruff is therefore the selected Part 0 frontend.

## Version Policy

Part 0 separates version support into three layers.

| Layer | v0 Policy |
|---|---|
| Grammar coverage | Ruff parser target versions through Python 3.14, with Python 3.15 as a canary. |
| Semantic support | Python 3.10 through 3.14 is the v0 semantic matrix. |
| Product support | Python 3.10 through 3.14 for v0, subject to later benchmark release gates. |

Part 0 includes a target-version diagnostic test to ensure newer syntax is reported when parsing
against an older configured version.

## Encoding Policy

Cull uses canonical UTF-8 internally and Python-compatible decoding at the boundary.

Implemented decoding behavior:

- detect an initial UTF-8 BOM
- inspect the first two physical lines for a Python coding declaration
- default to UTF-8 when no declaration exists
- support UTF-8, UTF-8 with BOM, ASCII, Latin-1, and ISO-8859-1 labels
- emit structured diagnostics for unsupported or invalid encodings

For decoded non-UTF-8 files, byte ranges refer to Cull's canonical UTF-8 source, not original
on-disk byte offsets. This is acceptable for v0 because Cull does not edit files.

## Verification

Part 0 verification includes:

- unit tests for decoding and module naming
- Ruff lowering tests for top-level definition extraction
- modern syntax corpus tests
- target-version diagnostic tests
- golden JSON snapshots for `src/` and flat layouts
- deterministic repeated-output check
- CPython `ast` oracle comparison for top-level definition ranges
- parser/lowering fuzz target at `fuzz/fuzz_targets/parse_definitions.rs`

The CPython oracle is implemented in `scripts/cpython_definition_oracle.py`.
