# Part 2 Implementation Note

Part 2 implements the first public project-wide check surface for `CULL001` and `CULL002`.

## Scope Implemented

- Module namespace index with path-entry-first provider precedence.
- Local namespace-package portion resolution.
- Cross-module import and module-attribute resolution using `BindingId` provenance.
- Synthetic parent-package submodule attributes as namespace binding events with order and
  partial-initialization uncertainty when order is not proven.
- Assignment aliases, static star imports, direct re-exports, aliased re-exports, and package
  public surface convergence through one fixed-point resolver.
- Module-exit `__all__`, including conditional absent-path implicit public surface for star imports.
- `auto`, `application`, and `library` modes, with `auto` as the default.
- Public `cull check` text and JSON output.
- Default text output limited to high-confidence findings.
- JSON output including `Review` findings.
- Exit codes:
  - `0`: no default-visible high-confidence findings
  - `1`: one or more default-visible high-confidence findings
  - `2`: configuration, discovery, decode, parse, or analysis failure

## Test Coverage

Focused Part 2 tests cover:

- path-entry-first shadowing of duplicate providers
- local namespace packages across source roots
- ordinary import forms and module attribute chains
- dynamic import provenance through stdlib import aliases
- local shadowing of `import_module`
- circular local import partial-initialization uncertainty
- explicit `__all__`, direct re-exports, aliased re-exports, and re-export chains
- package re-exports as definite exports independent of mode
- conditional `__all__` with an absent path
- package public surface behavior in `auto`, `application`, and `library` policy
- deterministic finding order across file creation order
- text, JSON, and exit-code behavior in the CLI

## Real-Repository Checkpoints

Checkpoint artifacts were written under `.context/part2/`.

| Domain | Repository | Tag | Resolved commit SHA | Mode | Included paths | Cull result |
|---|---|---|---|---|---|---|
| application | `adamchainz/django-upgrade` | `1.30.0` | `1db0cbd209d6c4dc78191593942e6269fae99e8d` | `application` | `src` | exit `0`, high `0`, review `134`, diagnostics `4` warnings |
| library | `pallets/itsdangerous` | `2.2.0` | `096c8d42545d3b68ea21a4f890fb2b2d8979c0bd` | `library` | `src` | exit `0`, high `0`, review `1`, diagnostics `0` |

The Django warnings were existing fail-closed semantic warnings for assignment expressions inside
comprehensions. No checkpoint emitted default-visible high-confidence findings, so there were no
known high-confidence cross-module false positives to adjudicate.

Smoke comparison tools:

- Vulture `2.16`
- `deadcode` `2.4.1`, run through `uvx --python 3.11` because the same version crashes under
  Python 3.14 due removed `ast.Str`

Summary counts:

| Repository | Cull high | Cull review | Vulture lines | `deadcode` lines |
|---|---:|---:|---:|---:|
| `django-upgrade` | 0 | 134 | 11 | 82 |
| `itsdangerous` | 0 | 1 | 6 | 6 |

Runtime and memory from `/usr/bin/time -l`:

| Repository | Tool | Runtime | Max RSS | Peak footprint |
|---|---|---:|---:|---:|
| `django-upgrade` | Cull | `0.50 real` | `18219008` | `11764072` |
| `django-upgrade` | Vulture | `0.11 real` | `36356096` | `15712760` |
| `django-upgrade` | `deadcode` | `0.60 real` | `46153728` | `20120152` |
| `itsdangerous` | Cull | `0.09 real` | `10764288` | `4391272` |
| `itsdangerous` | Vulture | `0.07 real` | `36323328` | `15679992` |
| `itsdangerous` | `deadcode` | `0.53 real` | `46350336` | `20333144` |

## Verification

Commands run:

```bash
cargo test
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
git diff --check
```
