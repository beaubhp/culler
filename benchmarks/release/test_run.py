#!/usr/bin/env python3
"""Unit tests for the dependency-free release benchmark runner."""

from __future__ import annotations

import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path


RUNNER_PATH = Path(__file__).with_name("run.py")
SPEC = importlib.util.spec_from_file_location("benchmark_run", RUNNER_PATH)
assert SPEC is not None
benchmark_run = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = benchmark_run
assert SPEC.loader is not None
SPEC.loader.exec_module(benchmark_run)


def metric(
    *,
    tp: int,
    fp: int,
    fn: int,
    expected_dead: int,
    unmatched: int = 0,
) -> dict[str, object]:
    precision = benchmark_run.ratio(tp, tp + fp)
    recall = benchmark_run.ratio(tp, tp + fn)
    return {
        "tp": tp,
        "fp": fp,
        "fn": fn,
        "expected_dead": expected_dead,
        "ignored": {"out_of_scope": 0, "test_only": 0, "unknown": 0},
        "unmatched": unmatched,
        "rule_mismatch": 0,
        "confidence_mismatch": 0,
        "precision": precision,
        "recall": recall,
        "f1": benchmark_run.f1(tp, fp, fn),
    }


class BenchmarkRunnerTests(unittest.TestCase):
    def test_baseline_parsers_normalize_paths_and_ansi(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            module = root / "src" / "pkg" / "mod.py"
            module.parent.mkdir(parents=True)
            module.write_text("def unused():\n    pass\n", encoding="utf-8")

            vulture = benchmark_run.parse_vulture(
                f"{module}:1: unused function 'unused' (60% confidence)\n",
                root,
            )
            deadcode = benchmark_run.parse_deadcode(
                f"\x1b[31m{module}:1:1: DC02 Function `unused` is never used\x1b[0m\n",
                root,
            )

        expected = benchmark_run.FindingKey("src/pkg/mod.py", 1, "function", "unused")
        self.assertEqual([finding.key for finding in vulture], [expected])
        self.assertEqual([finding.key for finding in deadcode], [expected])
        self.assertEqual(deadcode[0].rule_id, "DC02")

    def test_score_findings_counts_truth_fp_ignored_and_unmatched(self) -> None:
        labels = {
            "definitions": [
                {
                    "path": "src/pkg/mod.py",
                    "line": 1,
                    "kind": "function",
                    "qualified_name": "pkg.mod::dead",
                    "label": "dead",
                },
                {
                    "path": "src/pkg/mod.py",
                    "line": 5,
                    "kind": "function",
                    "qualified_name": "pkg.mod::live",
                    "label": "live",
                },
                {
                    "path": "src/pkg/mod.py",
                    "line": 9,
                    "kind": "class",
                    "qualified_name": "pkg.mod::TestOnly",
                    "label": "test_only",
                },
            ]
        }
        findings = [
            benchmark_run.ToolFinding(
                benchmark_run.FindingKey("src/pkg/mod.py", 1, "function", "dead"),
                "CULL001",
                "high",
            ),
            benchmark_run.ToolFinding(
                benchmark_run.FindingKey("src/pkg/mod.py", 5, "function", "live"),
                "CULL001",
                "high",
            ),
            benchmark_run.ToolFinding(
                benchmark_run.FindingKey("src/pkg/mod.py", 9, "class", "TestOnly"),
                "CULL002",
                "review",
            ),
            benchmark_run.ToolFinding(
                benchmark_run.FindingKey("src/pkg/other.py", 1, "function", "missing"),
                "CULL001",
                "high",
            ),
        ]

        score = benchmark_run.score_findings(labels, findings, filter_unmatched=False)

        self.assertEqual(score["tp"], 1)
        self.assertEqual(score["fp"], 1)
        self.assertEqual(score["fn"], 0)
        self.assertEqual(score["ignored"]["test_only"], 1)
        self.assertEqual(score["unmatched"], 1)
        self.assertEqual(score["rule_mismatch"], 0)
        self.assertEqual(score["confidence_mismatch"], 0)

    def test_score_findings_enforces_expected_rule_and_confidence_ceiling(self) -> None:
        labels = {
            "definitions": [
                {
                    "path": "src/pkg/mod.py",
                    "line": 1,
                    "kind": "function",
                    "qualified_name": "pkg.mod::dead",
                    "label": "dead",
                    "expected_rule": "CULL001",
                    "max_confidence": "review",
                }
            ]
        }
        findings = [
            benchmark_run.ToolFinding(
                benchmark_run.FindingKey("src/pkg/mod.py", 1, "function", "dead"),
                "CULL003",
                "high",
            )
        ]

        score = benchmark_run.score_findings(
            labels,
            findings,
            filter_unmatched=False,
            enforce_expected_rule=True,
            enforce_confidence=True,
        )

        self.assertEqual(score["tp"], 0)
        self.assertEqual(score["fn"], 1)
        self.assertEqual(score["rule_mismatch"], 1)
        self.assertEqual(score["confidence_mismatch"], 1)

    def test_gate_fails_on_unmatched_cull_findings(self) -> None:
        thresholds = {
            "high_confidence_false_positive_budget": 0,
            "min_labeled_dead": 2,
            "min_cull_reported_true_positives": 2,
            "min_cull_high_true_positives": 2,
            "min_cull_high_precision": 0.95,
            "min_cull_reported_recall": 0.9,
            "max_cull_case_seconds": 1.0,
            "max_cull_case_rss_mb": 512,
            "max_cull_full_corpus_seconds": 30.0,
            "min_holdout_labeled_dead": 2,
            "min_holdout_cull_high_true_positives": 2,
            "min_holdout_cull_reported_recall": 0.9,
            "required_modes": ["application"],
        }
        aggregate = {
            "labeled_dead": 2,
            "cull_seconds": 0.1,
            "tools": {
                "cull_high": metric(tp=2, fp=0, fn=0, expected_dead=2, unmatched=1),
                "cull_reported": metric(tp=2, fp=0, fn=0, expected_dead=2),
                "vulture": metric(tp=2, fp=1, fn=0, expected_dead=2),
                "deadcode": metric(tp=2, fp=1, fn=0, expected_dead=2),
            },
        }
        cases = [{"labels": {"dead": 2}, "cull": {"seconds": 0.1, "max_rss_bytes": 1}}]

        gate = benchmark_run.evaluate_gate(
            thresholds,
            aggregate,
            cases,
            {"application": aggregate},
            aggregate,
            require_baselines=True,
        )

        self.assertEqual(gate["status"], "fail")
        failed = {check["name"] for check in gate["checks"] if not check["pass"]}
        self.assertIn("Cull high-confidence unmatched findings", failed)

    def test_gate_requires_baseline_metrics(self) -> None:
        thresholds = {
            "high_confidence_false_positive_budget": 0,
            "min_labeled_dead": 2,
            "min_cull_reported_true_positives": 2,
            "min_cull_high_true_positives": 2,
            "min_cull_high_precision": 0.95,
            "min_cull_reported_recall": 0.9,
            "max_cull_case_seconds": 1.0,
            "max_cull_case_rss_mb": 512,
            "max_cull_full_corpus_seconds": 30.0,
            "min_holdout_labeled_dead": 2,
            "min_holdout_cull_high_true_positives": 2,
            "min_holdout_cull_reported_recall": 0.9,
            "required_modes": ["application"],
        }
        aggregate = {
            "labeled_dead": 2,
            "cull_seconds": 0.1,
            "tools": {
                "cull_high": metric(tp=2, fp=0, fn=0, expected_dead=2),
                "cull_reported": metric(tp=2, fp=0, fn=0, expected_dead=2),
                "vulture": metric(tp=0, fp=0, fn=0, expected_dead=0),
                "deadcode": metric(tp=0, fp=0, fn=0, expected_dead=0),
            },
        }
        cases = [{"labels": {"dead": 2}, "cull": {"seconds": 0.1, "max_rss_bytes": 1}}]

        gate = benchmark_run.evaluate_gate(
            thresholds,
            aggregate,
            cases,
            {"application": aggregate},
            aggregate,
            require_baselines=True,
        )

        self.assertEqual(gate["status"], "fail")
        failed = {check["name"] for check in gate["checks"] if not check["pass"]}
        self.assertIn("baseline metrics available", failed)

    def test_validate_suite_rejects_inadequate_real_repository_set(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            case_root = root / "fixtures" / "case"
            case_root.mkdir(parents=True)
            (case_root / "mod.py").write_text("def dead():\n    pass\n", encoding="utf-8")
            labels = root / "labels.json"
            labels.write_text(
                json.dumps(
                    {
                        "case_id": "case",
                        "definitions": [
                            {
                                "path": "mod.py",
                                "line": 1,
                                "kind": "function",
                                "qualified_name": "mod::dead",
                                "label": "dead",
                            }
                        ],
                    }
                ),
                encoding="utf-8",
            )
            suite = {
                "schema_version": 1,
                "cases": [
                    {
                        "id": "case",
                        "path": "fixtures/case",
                        "labels": "labels.json",
                        "holdout": True,
                    }
                ],
                "real_repositories": [{"id": "one"}, {"id": "two"}],
            }

            with self.assertRaisesRegex(SystemExit, "three to five real repositories"):
                benchmark_run.validate_suite(root, suite)

    def test_write_summary_includes_audit_sections(self) -> None:
        result = {
            "gate": {
                "status": "pass",
                "checks": [
                    {
                        "name": "minimum labeled dead definitions",
                        "pass": True,
                        "actual": 2,
                        "expected": 2,
                    }
                ],
            },
            "aggregate": {
                "case_count": 1,
                "labeled_dead": 2,
                "cull_seconds": 0.1,
                "cull_max_rss_mb": 1.5,
                "tools": {
                    "cull_high": metric(tp=2, fp=0, fn=0, expected_dead=2),
                    "cull_reported": metric(tp=2, fp=0, fn=0, expected_dead=2),
                    "vulture": metric(tp=1, fp=1, fn=1, expected_dead=2),
                    "deadcode": metric(tp=1, fp=1, fn=1, expected_dead=2),
                },
            },
            "cases": [
                {
                    "id": "case",
                    "mode": "application",
                    "cull": {
                        "summary": {"high_confidence": 2, "review": 1, "suppressed": 3},
                        "seconds": 0.1,
                        "max_rss_bytes": 1_500_000,
                        "output_hash": "abcdef0123456789",
                    },
                }
            ],
            "mode_aggregates": {
                "application": {
                    "tools": {
                        "cull_high": metric(tp=2, fp=0, fn=0, expected_dead=2),
                        "cull_reported": metric(tp=2, fp=0, fn=0, expected_dead=2),
                        "vulture": metric(tp=1, fp=1, fn=1, expected_dead=2),
                        "deadcode": metric(tp=1, fp=1, fn=1, expected_dead=2),
                    }
                }
            },
            "holdout_aggregate": {
                "tools": {
                    "cull_high": metric(tp=2, fp=0, fn=0, expected_dead=2),
                    "cull_reported": metric(tp=2, fp=0, fn=0, expected_dead=2),
                    "vulture": metric(tp=1, fp=1, fn=1, expected_dead=2),
                    "deadcode": metric(tp=1, fp=1, fn=1, expected_dead=2),
                }
            },
            "real_repositories": [],
        }
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "summary.md"
            benchmark_run.write_summary(path, result)
            summary = path.read_text(encoding="utf-8")

        self.assertIn("Truth-Corpus Metrics", summary)
        self.assertIn("Mode Metrics", summary)
        self.assertIn("Holdout Metrics", summary)
        self.assertIn("Runtime And Volume", summary)
        self.assertIn("Unmatched", summary)
        self.assertIn("suppressed", summary.lower())

    def test_normalize_artifact_paths_removes_local_repo_prefix(self) -> None:
        repo_root = Path("/workspace/cull")
        value = {
            "command": [
                "/workspace/cull/target/release/cull",
                "/workspace/cull/benchmarks/release/fixtures/case",
            ],
            "stdout": "/workspace/cull/benchmarks/release/fixtures/case/src/pkg/mod.py:1: finding\n",
            "nested": [
                {"path": "/workspace/cull/.context/benchmarks/release/real/pkg"}
            ],
            "external": "/usr/bin/time",
        }

        normalized = benchmark_run.normalize_artifact_paths(value, repo_root)

        self.assertEqual(
            normalized["command"],
            ["target/release/cull", "benchmarks/release/fixtures/case"],
        )
        self.assertEqual(
            normalized["stdout"],
            "benchmarks/release/fixtures/case/src/pkg/mod.py:1: finding\n",
        )
        self.assertEqual(
            normalized["nested"],
            [{"path": ".context/benchmarks/release/real/pkg"}],
        )
        self.assertEqual(normalized["external"], "/usr/bin/time")

    def test_refresh_artifact_hashes_uses_normalized_stdout(self) -> None:
        result = {
            "cases": [
                {
                    "baselines": {
                        "vulture": {
                            "stdout": "benchmarks/release/fixtures/case/src/pkg/mod.py:1\n",
                            "output_hash": "stale",
                        }
                    }
                }
            ],
            "real_repositories": [],
        }

        benchmark_run.refresh_artifact_hashes(result)

        self.assertEqual(
            result["cases"][0]["baselines"]["vulture"]["output_hash"],
            benchmark_run.text_hash(
                "benchmarks/release/fixtures/case/src/pkg/mod.py:1\n"
            ),
        )


if __name__ == "__main__":
    unittest.main()
