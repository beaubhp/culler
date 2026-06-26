# Contributing

Culler is intentionally small and precise. Changes should keep the analyzer
fast, conservative where Python is dynamic, and explicit where evidence is
strong.

## Development Checks

Run the relevant checks before opening a pull request:

```bash
cargo fmt --all --check
cargo check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
python3 benchmark/test_run.py
python3 benchmark/run.py --validate-only
```

Packaging changes should also run:

```bash
uvx maturin build --release --out dist --sdist
uvx twine check dist/*
```

## Commit Discipline

Culler uses Conventional Commits so release automation can generate accurate
release notes and version bumps.

Use one of these prefixes:

| Prefix | Use for |
| --- | --- |
| `feat` | New analyzer behavior, rules, CLI options, or user-visible capabilities. |
| `fix` | Correctness fixes, false-positive fixes, false-negative fixes, or broken behavior. |
| `perf` | Runtime or memory improvements without behavior loss. |
| `docs` | README, benchmark explanation, comments, or contributor documentation. |
| `test` | Test-only changes. |
| `refactor` | Internal cleanup without intended behavior change. |
| `build` | Packaging, dependency, or build-system changes. |
| `ci` | GitHub Actions or release automation changes. |
| `chore` | Maintenance that does not fit the categories above. |

Good examples:

```text
feat: detect unused private methods
fix: preserve exported private helpers in library mode
perf: avoid duplicate import graph walks
docs: document root coverage modes
ci: build PyPI wheels on release tags
```

Avoid vague messages:

```text
update files
misc fixes
work
changes
final cleanup
```

For AI-generated commits, prefer a specific one-line summary of the actual
behavior or repository surface changed. If a change spans multiple concerns,
split it before committing. See [`COMMIT_MESSAGE.md`](COMMIT_MESSAGE.md) for
the repository's commit-message template.

## Versioning

Culler is currently `0.x`.

- `feat` commits normally become minor releases.
- `fix` and `perf` commits normally become patch releases.
- Breaking changes are allowed before `1.0`, but they must be called out in the
  changelog.
- Publishing is never triggered directly from ordinary `main` commits.

The Cargo workspace version is the source of truth. Python package metadata
derives its version from Cargo through Maturin.

Release Please opens reviewed release PRs from Conventional Commits. Those PRs
update `CHANGELOG.md` and `workspace.package.version`; merging the release PR
creates the GitHub release. The release workflow then builds wheels and
publishes to PyPI through Trusted Publishing.

## Pull Request Expectations

Pull requests should include:

- a focused description of the change;
- tests or a clear explanation of why tests are not needed;
- benchmark notes when behavior or performance changes materially;
- no unrelated formatting, generated files, or benchmark result artifacts.
