#!/usr/bin/env python3
"""Run the Cull release benchmark suite.

The runner is intentionally dependency-free so benchmark metadata, labels, raw
tool outputs, and summary metrics remain reproducible in ordinary local and CI
environments. It scores only the current reportable unit of analysis: top-level Python functions
and classes represented in the label files.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any


SCORED_LABELS = {"dead", "live"}
EXCLUDED_LABELS = {"unknown", "test_only", "out_of_scope"}
CONFIDENCE_RANK = {"review": 1, "high": 2}
ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")
VULTURE_RE = re.compile(
    r"^(?P<path>.*?):(?P<line>\d+): unused "
    r"(?P<kind>function|class) '(?P<name>[^']+)'"
)
DEADCODE_RE = re.compile(
    r"^(?P<path>.*?):(?P<line>\d+):(?P<column>\d+): "
    r"(?P<code>DC02|DC03) (?P<kind>Function|Class) `(?P<name>[^`]+)` is never used"
)


@dataclass(frozen=True, order=True)
class FindingKey:
    path: str
    line: int
    kind: str
    name: str


@dataclass(frozen=True)
class ToolFinding:
    key: FindingKey
    rule_id: str | None
    confidence: str | None


@dataclass
class CommandResult:
    args: list[str]
    exit_code: int
    stdout: str
    stderr: str
    seconds: float
    max_rss_bytes: int | None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--suite", default="suite.json", help="suite metadata JSON")
    parser.add_argument("--cull", default="target/release/cull", help="Cull binary")
    parser.add_argument("--results", default="results/latest.json", help="result JSON path")
    parser.add_argument("--real-root", default=".context/benchmarks/release/real")
    parser.add_argument("--include-real", action="store_true")
    parser.add_argument("--skip-baselines", action="store_true")
    parser.add_argument("--validate-only", action="store_true")
    args = parser.parse_args()

    root = Path(__file__).resolve().parent
    repo_root = root.parents[1]
    suite = load_json(root / args.suite)
    validate_suite(root, suite)
    if args.validate_only:
        print("benchmark suite metadata is valid")
        return 0

    cull = (repo_root / args.cull).resolve()
    if not cull.exists():
        raise SystemExit(f"Cull binary not found: {cull}")

    result = run_suite(
        root=root,
        suite=suite,
        cull=cull,
        include_real=args.include_real,
        skip_baselines=args.skip_baselines,
        real_root=(repo_root / args.real_root).resolve(),
    )
    result = normalize_artifact_paths(result, repo_root)
    refresh_artifact_hashes(result)
    result_path = root / args.results
    result_path.parent.mkdir(parents=True, exist_ok=True)
    write_json(result_path, result)
    write_summary(result_path.with_suffix(".md"), result)
    print(f"wrote {result_path}")
    print(f"gate status: {result['gate']['status']}")
    return 0 if result["gate"]["status"] == "pass" else 1


def run_suite(
    *,
    root: Path,
    suite: dict[str, Any],
    cull: Path,
    include_real: bool,
    skip_baselines: bool,
    real_root: Path,
) -> dict[str, Any]:
    cases = [
        run_case(
            root=root,
            case=case,
            cull=cull,
            skip_baselines=skip_baselines,
            is_real=False,
        )
        for case in suite["cases"]
    ]
    real_cases: list[dict[str, Any]] = []
    if include_real:
        real_root.mkdir(parents=True, exist_ok=True)
        for case in suite.get("real_repositories", []):
            checkout = ensure_real_checkout(real_root, case)
            case_with_path = dict(case)
            case_with_path["path"] = os.fspath(checkout)
            real_cases.append(
                run_case(
                    root=root,
                    case=case_with_path,
                    cull=cull,
                    skip_baselines=skip_baselines,
                    is_real=True,
                )
            )

    aggregate = aggregate_truth_cases(cases)
    mode_aggregates = aggregate_by_mode(cases)
    holdout_aggregate = aggregate_truth_cases(
        [case for case in cases if case.get("holdout", False)]
    )
    gate = evaluate_gate(
        suite["thresholds"],
        aggregate,
        cases,
        mode_aggregates,
        holdout_aggregate,
        require_baselines=True,
    )
    return {
        "schema_version": 1,
        "suite": suite["description"],
        "tools": suite["tools"],
        "thresholds": suite["thresholds"],
        "aggregate": aggregate,
        "mode_aggregates": mode_aggregates,
        "holdout_aggregate": holdout_aggregate,
        "gate": gate,
        "cases": cases,
        "real_repositories": real_cases,
        "environment": {
            "python": sys.version.split()[0],
            "platform": sys.platform,
        },
    }


def run_case(
    *,
    root: Path,
    case: dict[str, Any],
    cull: Path,
    skip_baselines: bool,
    is_real: bool,
) -> dict[str, Any]:
    case_root = Path(case["path"])
    if not case_root.is_absolute():
        case_root = root / case_root
    case_root = case_root.resolve()
    labels = load_labels(root, case) if not is_real else None
    adjudication = load_json(root / case["adjudication"]) if is_real else None

    source_roots = [case_root / source for source in case["source_roots"]]
    target_files = collect_python_target_files(case_root, source_roots)
    cull_check = run_cull_check(cull, case_root, case)
    cull_debug = run_cull_debug(cull, case_root, case)
    cull_output = parse_json_or_empty(cull_check.stdout)
    debug_output = parse_json_or_empty(cull_debug.stdout)
    cull_findings = cull_tool_findings(cull_output)
    cull_high = [finding for finding in cull_findings if finding.confidence == "high"]
    cull_reported = [
        finding
        for finding in cull_findings
        if finding.confidence in {"high", "review"}
    ]

    result: dict[str, Any] = {
        "id": case["id"],
        "kind": case["kind"],
        "mode": case["mode"],
        "holdout": bool(case.get("holdout", False)),
        "source_roots": case["source_roots"],
        "target_files": target_files,
        "cull": {
            "command": cull_check.args,
            "exit_code": cull_check.exit_code,
            "seconds": cull_check.seconds,
            "max_rss_bytes": cull_check.max_rss_bytes,
            "output_hash": stable_json_hash(cull_output),
            "summary": cull_output.get("summary", {}),
            "diagnostics": cull_output.get("diagnostics", []),
            "review": cull_output.get("summary", {}).get("review", 0),
            "suppressed": cull_output.get("summary", {}).get("suppressed", 0),
        },
        "debug_candidates": {
            "command": cull_debug.args,
            "exit_code": cull_debug.exit_code,
            "output_hash": stable_json_hash(debug_output),
            "candidate_count": len(debug_output.get("candidates", [])),
        },
        "baselines": {},
    }
    if adjudication is not None:
        result["adjudication"] = adjudication

    if labels is not None:
        result["labels"] = label_summary(labels)
        result["metrics"] = {
            "cull_high": score_findings(
                labels,
                cull_high,
                filter_unmatched=False,
                enforce_expected_rule=True,
                enforce_confidence=True,
            ),
            "cull_reported": score_findings(
                labels,
                cull_reported,
                filter_unmatched=False,
                enforce_expected_rule=True,
                enforce_confidence=True,
            ),
        }

    if case.get("run_baselines", True) and not skip_baselines:
        vulture = run_vulture(source_roots)
        deadcode = run_deadcode(source_roots)
        vulture_findings = parse_vulture(vulture.stdout, case_root)
        deadcode_findings = parse_deadcode(deadcode.stdout, case_root)
        result["baselines"] = {
            "vulture": baseline_record(
                vulture, vulture_findings, labels, target_files=target_files
            ),
            "deadcode": baseline_record(
                deadcode, deadcode_findings, labels, target_files=target_files
            ),
        }
    return result


def collect_python_target_files(case_root: Path, source_roots: list[Path]) -> list[str]:
    files: set[str] = set()
    for source_root in source_roots:
        if source_root.is_file() and source_root.suffix == ".py":
            files.add(relative_to_root(source_root, case_root))
            continue
        if not source_root.exists():
            continue
        for path in source_root.rglob("*.py"):
            if path.is_file():
                files.add(relative_to_root(path, case_root))
    return sorted(files)


def run_cull_check(cull: Path, case_root: Path, case: dict[str, Any]) -> CommandResult:
    args = [
        os.fspath(cull),
        "check",
        os.fspath(case_root),
        "--format",
        "json",
        "--mode",
        case["mode"],
    ]
    for source in case["source_roots"]:
        args.extend(["--src", source])
    if case.get("allow_partial", False):
        args.append("--allow-partial")
    return run_timed(args)


def run_cull_debug(cull: Path, case_root: Path, case: dict[str, Any]) -> CommandResult:
    args = [
        os.fspath(cull),
        "debug",
        "candidates",
        os.fspath(case_root),
        "--format",
        "json",
        "--mode",
        case["mode"],
    ]
    for source in case["source_roots"]:
        args.extend(["--src", source])
    if case.get("allow_partial", False):
        args.append("--allow-partial")
    return run_timed(args)


def run_vulture(source_roots: list[Path]) -> CommandResult:
    args = ["uvx", "vulture==2.16", *map(os.fspath, source_roots)]
    return run_timed(args)


def run_deadcode(source_roots: list[Path]) -> CommandResult:
    args = [
        "uvx",
        "--python",
        "3.11",
        "deadcode==2.4.1",
        *map(os.fspath, source_roots),
    ]
    return run_timed(args)


def run_timed(args: list[str]) -> CommandResult:
    if Path("/usr/bin/time").exists():
        timed_args = ["/usr/bin/time", "-l", *args]
        start = time.perf_counter()
        completed = subprocess.run(
            timed_args,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        seconds = time.perf_counter() - start
        stderr, max_rss = split_time_stderr(completed.stderr)
        return CommandResult(
            args=args,
            exit_code=completed.returncode,
            stdout=completed.stdout,
            stderr=stderr,
            seconds=seconds,
            max_rss_bytes=max_rss,
        )

    start = time.perf_counter()
    completed = subprocess.run(
        args,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    return CommandResult(
        args=args,
        exit_code=completed.returncode,
        stdout=completed.stdout,
        stderr=completed.stderr,
        seconds=time.perf_counter() - start,
        max_rss_bytes=None,
    )


def split_time_stderr(stderr: str) -> tuple[str, int | None]:
    max_rss = None
    tool_lines = []
    for line in stderr.splitlines():
        stripped = line.strip()
        if stripped.endswith("maximum resident set size"):
            try:
                max_rss = int(stripped.split()[0])
            except (IndexError, ValueError):
                max_rss = None
            continue
        if re.match(r"^\d+\.\d+ real\s+\d+\.\d+ user\s+\d+\.\d+ sys$", stripped):
            continue
        if any(
            stripped.endswith(suffix)
            for suffix in (
                "average shared memory size",
                "average unshared data size",
                "average unshared stack size",
                "page reclaims",
                "page faults",
                "swaps",
                "block input operations",
                "block output operations",
                "messages sent",
                "messages received",
                "signals received",
                "voluntary context switches",
                "involuntary context switches",
                "instructions retired",
                "cycles elapsed",
                "peak memory footprint",
            )
        ):
            continue
        tool_lines.append(line)
    return "\n".join(tool_lines), max_rss


def load_labels(root: Path, case: dict[str, Any]) -> dict[str, Any]:
    return load_json(root / case["labels"])


def label_summary(labels: dict[str, Any]) -> dict[str, int]:
    counts: dict[str, int] = {}
    for definition in labels["definitions"]:
        counts[definition["label"]] = counts.get(definition["label"], 0) + 1
    return counts


def cull_tool_findings(output: dict[str, Any]) -> list[ToolFinding]:
    findings = []
    for finding in output.get("findings", []):
        definition = finding["definition"]
        findings.append(
            ToolFinding(
                key=FindingKey(
                    path=definition["file"],
                    line=int(definition["line"]),
                    kind=definition["kind"],
                    name=definition["name"],
                ),
                rule_id=finding.get("rule_id"),
                confidence=finding.get("confidence"),
            )
        )
    return findings


def parse_vulture(stdout: str, root: Path) -> list[ToolFinding]:
    findings = []
    for line in stdout.splitlines():
        match = VULTURE_RE.match(line.strip())
        if not match:
            continue
        path = relative_to_root(Path(match.group("path")), root)
        findings.append(
            ToolFinding(
                key=FindingKey(
                    path=path,
                    line=int(match.group("line")),
                    kind=match.group("kind"),
                    name=match.group("name"),
                ),
                rule_id=None,
                confidence=None,
            )
        )
    return findings


def parse_deadcode(stdout: str, root: Path) -> list[ToolFinding]:
    findings = []
    for raw_line in stdout.splitlines():
        line = ANSI_RE.sub("", raw_line.strip())
        match = DEADCODE_RE.match(line)
        if not match:
            continue
        path = relative_to_root(Path(match.group("path")), root)
        findings.append(
            ToolFinding(
                key=FindingKey(
                    path=path,
                    line=int(match.group("line")),
                    kind=match.group("kind").lower(),
                    name=match.group("name"),
                ),
                rule_id=match.group("code"),
                confidence=None,
            )
        )
    return findings


def relative_to_root(path: Path, root: Path) -> str:
    try:
        return os.fspath(path.resolve().relative_to(root.resolve()))
    except ValueError:
        return os.fspath(path)


def score_findings(
    labels: dict[str, Any],
    findings: list[ToolFinding],
    *,
    filter_unmatched: bool,
    enforce_expected_rule: bool = False,
    enforce_confidence: bool = False,
) -> dict[str, Any]:
    label_by_key = {
        FindingKey(
            path=definition["path"],
            line=int(definition["line"]),
            kind=definition["kind"],
            name=definition["qualified_name"].split("::")[-1],
        ): definition
        for definition in labels["definitions"]
    }
    expected_dead = {
        key for key, definition in label_by_key.items() if definition["label"] == "dead"
    }
    true_positive: set[FindingKey] = set()
    false_positive: set[FindingKey] = set()
    ignored: dict[str, int] = {label: 0 for label in sorted(EXCLUDED_LABELS)}
    rule_mismatch: list[str] = []
    confidence_mismatch: list[str] = []
    unmatched = 0

    for finding in findings:
        definition = label_by_key.get(finding.key)
        if definition is None:
            if not filter_unmatched:
                unmatched += 1
            continue
        label = definition["label"]
        confidence_issue = confidence_mismatch_detail(definition, finding)
        if enforce_confidence and confidence_issue is not None:
            confidence_mismatch.append(confidence_issue)
        if label == "dead":
            expected_rule = definition.get("expected_rule")
            if (
                enforce_expected_rule
                and expected_rule is not None
                and finding.rule_id != expected_rule
            ):
                rule_mismatch.append(
                    "{key} expected={expected} actual={actual}".format(
                        key=key_to_string(finding.key),
                        expected=expected_rule,
                        actual=finding.rule_id,
                    )
                )
            else:
                true_positive.add(finding.key)
        elif label == "live":
            false_positive.add(finding.key)
        else:
            ignored[label] = ignored.get(label, 0) + 1

    false_negative = expected_dead - true_positive
    tp = len(true_positive)
    fp = len(false_positive)
    fn = len(false_negative)
    return {
        "tp": tp,
        "fp": fp,
        "fn": fn,
        "precision": ratio(tp, tp + fp),
        "recall": ratio(tp, tp + fn),
        "f1": f1(tp, fp, fn),
        "expected_dead": len(expected_dead),
        "ignored": ignored,
        "unmatched": unmatched,
        "rule_mismatch": len(rule_mismatch),
        "confidence_mismatch": len(confidence_mismatch),
        "true_positive": sorted(key_to_string(key) for key in true_positive),
        "false_positive": sorted(key_to_string(key) for key in false_positive),
        "false_negative": sorted(key_to_string(key) for key in false_negative),
        "rule_mismatches": sorted(rule_mismatch),
        "confidence_mismatches": sorted(confidence_mismatch),
    }


def confidence_mismatch_detail(
    definition: dict[str, Any],
    finding: ToolFinding,
) -> str | None:
    actual = finding.confidence
    if actual is None:
        return None
    key = key_to_string(finding.key)
    expected = definition.get("expected_confidence")
    if expected is not None and actual != expected:
        return f"{key} expected_confidence={expected} actual={actual}"
    max_confidence = definition.get("max_confidence")
    if max_confidence is None:
        return None
    actual_rank = CONFIDENCE_RANK.get(actual)
    max_rank = CONFIDENCE_RANK.get(max_confidence)
    if actual_rank is not None and max_rank is not None and actual_rank > max_rank:
        return f"{key} max_confidence={max_confidence} actual={actual}"
    return None


def baseline_record(
    command: CommandResult,
    findings: list[ToolFinding],
    labels: dict[str, Any] | None,
    *,
    target_files: list[str],
) -> dict[str, Any]:
    record = {
        "command": command.args,
        "exit_code": command.exit_code,
        "seconds": command.seconds,
        "max_rss_bytes": command.max_rss_bytes,
        "target_files": target_files,
        "excludes": [],
        "stdout": command.stdout,
        "stderr": command.stderr,
        "output_hash": text_hash(command.stdout),
        "finding_count": len(findings),
    }
    if labels is not None:
        record["metrics"] = score_findings(labels, findings, filter_unmatched=True)
    return record


def aggregate_truth_cases(cases: list[dict[str, Any]]) -> dict[str, Any]:
    aggregate = {
        "case_count": 0,
        "labeled_dead": 0,
        "cull_seconds": 0.0,
        "cull_max_rss_bytes": 0,
        "tools": {
            "cull_high": empty_metric_sum(),
            "cull_reported": empty_metric_sum(),
            "vulture": empty_metric_sum(),
            "deadcode": empty_metric_sum(),
        },
    }
    for case in cases:
        if not case.get("labels") or not case.get("metrics"):
            continue
        aggregate["case_count"] += 1
        aggregate["labeled_dead"] += case["labels"].get("dead", 0)
        aggregate["cull_seconds"] += case["cull"]["seconds"]
        if case["cull"]["max_rss_bytes"] is not None:
            aggregate["cull_max_rss_bytes"] = max(
                aggregate["cull_max_rss_bytes"], case["cull"]["max_rss_bytes"]
            )
        add_metric_sum(aggregate["tools"]["cull_high"], case["metrics"]["cull_high"])
        add_metric_sum(
            aggregate["tools"]["cull_reported"], case["metrics"]["cull_reported"]
        )
        for baseline in ("vulture", "deadcode"):
            metric = case.get("baselines", {}).get(baseline, {}).get("metrics")
            if metric:
                add_metric_sum(aggregate["tools"][baseline], metric)

    for metric in aggregate["tools"].values():
        finalize_metric(metric)
    aggregate["cull_max_rss_mb"] = bytes_to_mb(aggregate["cull_max_rss_bytes"])
    return aggregate


def aggregate_by_mode(cases: list[dict[str, Any]]) -> dict[str, Any]:
    modes = sorted({case["mode"] for case in cases if case.get("labels")})
    return {
        mode: aggregate_truth_cases(
            [case for case in cases if case.get("labels") and case["mode"] == mode]
        )
        for mode in modes
    }


def empty_metric_sum() -> dict[str, Any]:
    return {
        "tp": 0,
        "fp": 0,
        "fn": 0,
        "expected_dead": 0,
        "ignored": {label: 0 for label in sorted(EXCLUDED_LABELS)},
        "unmatched": 0,
        "rule_mismatch": 0,
        "confidence_mismatch": 0,
    }


def add_metric_sum(target: dict[str, Any], metric: dict[str, Any]) -> None:
    for field in (
        "tp",
        "fp",
        "fn",
        "expected_dead",
        "unmatched",
        "rule_mismatch",
        "confidence_mismatch",
    ):
        target[field] += metric[field]
    for label, count in metric["ignored"].items():
        target["ignored"][label] = target["ignored"].get(label, 0) + count


def finalize_metric(metric: dict[str, Any]) -> None:
    tp = metric["tp"]
    fp = metric["fp"]
    fn = metric["fn"]
    metric["precision"] = ratio(tp, tp + fp)
    metric["recall"] = ratio(tp, tp + fn)
    metric["f1"] = f1(tp, fp, fn)


def evaluate_gate(
    thresholds: dict[str, Any],
    aggregate: dict[str, Any],
    cases: list[dict[str, Any]],
    mode_aggregates: dict[str, Any],
    holdout_aggregate: dict[str, Any],
    *,
    require_baselines: bool,
) -> dict[str, Any]:
    checks = []
    cull_high = aggregate["tools"]["cull_high"]
    cull_reported = aggregate["tools"]["cull_reported"]
    baseline_available = all(
        aggregate["tools"][tool]["expected_dead"] > 0 for tool in ("vulture", "deadcode")
    )
    baseline_precision = [
        aggregate["tools"][tool]["precision"]
        for tool in ("vulture", "deadcode")
        if aggregate["tools"][tool]["tp"] + aggregate["tools"][tool]["fp"] > 0
    ]
    required_modes = set(thresholds.get("required_modes", []))
    present_modes = set(mode_aggregates)
    max_case_seconds = max((case["cull"]["seconds"] for case in cases), default=0.0)
    max_case_rss = max(
        (
            case["cull"]["max_rss_bytes"] or 0
            for case in cases
            if case.get("labels")
        ),
        default=0,
    )
    checks.append(
        gate_check(
            "minimum labeled dead definitions",
            aggregate["labeled_dead"] >= thresholds["min_labeled_dead"],
            aggregate["labeled_dead"],
            thresholds["min_labeled_dead"],
        )
    )
    checks.append(
        gate_check(
            "required truth-corpus modes",
            required_modes <= present_modes,
            sorted(present_modes),
            sorted(required_modes),
        )
    )
    if require_baselines:
        checks.append(
            gate_check(
                "baseline metrics available",
                baseline_available,
                baseline_available,
                True,
            )
        )
    checks.append(
        gate_check(
            "Cull reported true-positive floor",
            cull_reported["tp"] >= thresholds["min_cull_reported_true_positives"],
            cull_reported["tp"],
            thresholds["min_cull_reported_true_positives"],
        )
    )
    checks.append(
        gate_check(
            "Cull high-confidence true-positive floor",
            cull_high["tp"] >= thresholds["min_cull_high_true_positives"],
            cull_high["tp"],
            thresholds["min_cull_high_true_positives"],
        )
    )
    checks.append(
        gate_check(
            "Cull high-confidence false-positive budget",
            cull_high["fp"] <= thresholds["high_confidence_false_positive_budget"],
            cull_high["fp"],
            thresholds["high_confidence_false_positive_budget"],
        )
    )
    checks.append(
        gate_check(
            "Cull high-confidence unmatched findings",
            cull_high["unmatched"] == 0,
            cull_high["unmatched"],
            0,
        )
    )
    checks.append(
        gate_check(
            "Cull reported unmatched findings",
            cull_reported["unmatched"] == 0,
            cull_reported["unmatched"],
            0,
        )
    )
    checks.append(
        gate_check(
            "Cull high-confidence ignored-label findings",
            sum(cull_high["ignored"].values()) == 0,
            sum(cull_high["ignored"].values()),
            0,
        )
    )
    checks.append(
        gate_check(
            "Cull high-confidence rule mismatches",
            cull_high["rule_mismatch"] == 0,
            cull_high["rule_mismatch"],
            0,
        )
    )
    checks.append(
        gate_check(
            "Cull reported rule mismatches",
            cull_reported["rule_mismatch"] == 0,
            cull_reported["rule_mismatch"],
            0,
        )
    )
    checks.append(
        gate_check(
            "Cull high-confidence confidence mismatches",
            cull_high["confidence_mismatch"] == 0,
            cull_high["confidence_mismatch"],
            0,
        )
    )
    checks.append(
        gate_check(
            "Cull reported confidence mismatches",
            cull_reported["confidence_mismatch"] == 0,
            cull_reported["confidence_mismatch"],
            0,
        )
    )
    checks.append(
        gate_check(
            "Cull high-confidence precision",
            cull_high["precision"] >= thresholds["min_cull_high_precision"],
            cull_high["precision"],
            thresholds["min_cull_high_precision"],
        )
    )
    checks.append(
        gate_check(
            "Cull reported recall",
            cull_reported["recall"] >= thresholds["min_cull_reported_recall"],
            cull_reported["recall"],
            thresholds["min_cull_reported_recall"],
        )
    )
    holdout_high = holdout_aggregate["tools"]["cull_high"]
    holdout_reported = holdout_aggregate["tools"]["cull_reported"]
    checks.append(
        gate_check(
            "holdout labeled dead definitions",
            holdout_aggregate["labeled_dead"] >= thresholds["min_holdout_labeled_dead"],
            holdout_aggregate["labeled_dead"],
            thresholds["min_holdout_labeled_dead"],
        )
    )
    checks.append(
        gate_check(
            "holdout Cull high-confidence true-positive floor",
            holdout_high["tp"] >= thresholds["min_holdout_cull_high_true_positives"],
            holdout_high["tp"],
            thresholds["min_holdout_cull_high_true_positives"],
        )
    )
    checks.append(
        gate_check(
            "holdout Cull high-confidence false-positive budget",
            holdout_high["fp"] <= thresholds["high_confidence_false_positive_budget"],
            holdout_high["fp"],
            thresholds["high_confidence_false_positive_budget"],
        )
    )
    checks.append(
        gate_check(
            "holdout Cull reported recall",
            holdout_reported["recall"] >= thresholds["min_holdout_cull_reported_recall"],
            holdout_reported["recall"],
            thresholds["min_holdout_cull_reported_recall"],
        )
    )
    if baseline_precision:
        best_baseline_precision = max(baseline_precision)
        checks.append(
            gate_check(
                "Cull high-confidence precision beats baselines",
                cull_high["precision"] > best_baseline_precision,
                cull_high["precision"],
                best_baseline_precision,
            )
        )
    checks.append(
        gate_check(
            "max Cull case runtime seconds",
            max_case_seconds <= thresholds["max_cull_case_seconds"],
            max_case_seconds,
            thresholds["max_cull_case_seconds"],
        )
    )
    checks.append(
        gate_check(
            "max Cull case RSS MiB",
            bytes_to_mb(max_case_rss) <= thresholds["max_cull_case_rss_mb"],
            bytes_to_mb(max_case_rss),
            thresholds["max_cull_case_rss_mb"],
        )
    )
    checks.append(
        gate_check(
            "Cull full truth-corpus runtime seconds",
            aggregate["cull_seconds"] <= thresholds["max_cull_full_corpus_seconds"],
            aggregate["cull_seconds"],
            thresholds["max_cull_full_corpus_seconds"],
        )
    )
    return {
        "status": "pass" if all(check["pass"] for check in checks) else "fail",
        "checks": checks,
    }


def gate_check(name: str, passed: bool, actual: Any, expected: Any) -> dict[str, Any]:
    return {"name": name, "pass": passed, "actual": actual, "expected": expected}


def ensure_real_checkout(real_root: Path, case: dict[str, Any]) -> Path:
    checkout = real_root / case["id"]
    if checkout.exists():
        current = subprocess.run(
            ["git", "-C", os.fspath(checkout), "rev-parse", "HEAD"],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        if current.returncode == 0 and current.stdout.strip() == case["commit"]:
            return checkout
        shutil.rmtree(checkout)
    subprocess.run(
        ["git", "clone", "--no-checkout", case["url"], os.fspath(checkout)],
        check=True,
    )
    subprocess.run(
        ["git", "-C", os.fspath(checkout), "checkout", case["commit"]],
        check=True,
    )
    return checkout


def validate_suite(root: Path, suite: dict[str, Any]) -> None:
    if suite.get("schema_version") != 1:
        raise SystemExit("unsupported suite schema_version")
    real_repository_count = len(suite.get("real_repositories", []))
    if not 3 <= real_repository_count <= 5:
        raise SystemExit("suite must define three to five real repositories")
    for case in suite.get("real_repositories", []):
        adjudication_path = root / case["adjudication"]
        if not adjudication_path.exists():
            raise SystemExit(f"missing real-repository adjudication: {adjudication_path}")
        adjudication = load_json(adjudication_path)
        if adjudication.get("repository_id") != case["id"]:
            raise SystemExit(f"adjudication repository mismatch for {case['id']}")
        if adjudication.get("commit") != case["commit"]:
            raise SystemExit(f"adjudication commit mismatch for {case['id']}")
    case_ids = set()
    holdout_count = 0
    for case in suite["cases"]:
        case_id = case["id"]
        if case_id in case_ids:
            raise SystemExit(f"duplicate case id: {case_id}")
        case_ids.add(case_id)
        if case.get("holdout", False):
            holdout_count += 1
        case_path = root / case["path"]
        if not case_path.exists():
            raise SystemExit(f"missing case path: {case_path}")
        labels = load_json(root / case["labels"])
        if labels["case_id"] != case_id:
            raise SystemExit(f"label case mismatch for {case_id}")
        validate_labels(case_path, labels)
    if holdout_count == 0:
        raise SystemExit("suite must define at least one constructed holdout")


def validate_labels(case_path: Path, labels: dict[str, Any]) -> None:
    seen = set()
    for definition in labels["definitions"]:
        if definition["label"] not in SCORED_LABELS | EXCLUDED_LABELS:
            raise SystemExit(f"invalid label: {definition}")
        if definition["label"] == "dead" and "expected_rule" not in definition:
            raise SystemExit(f"dead label missing expected_rule: {definition}")
        for field in ("expected_confidence", "max_confidence"):
            if field in definition and definition[field] not in CONFIDENCE_RANK:
                raise SystemExit(f"invalid {field}: {definition}")
        key = (
            definition["path"],
            definition["line"],
            definition["kind"],
            definition["qualified_name"],
        )
        if key in seen:
            raise SystemExit(f"duplicate label: {definition}")
        seen.add(key)
        path = case_path / definition["path"]
        if not path.exists():
            raise SystemExit(f"label references missing file: {path}")


def parse_json_or_empty(text: str) -> dict[str, Any]:
    if not text.strip():
        return {}
    return json.loads(text)


def load_json(path: Path) -> dict[str, Any]:
    with path.open("r", encoding="utf-8") as handle:
        return json.load(handle)


def normalize_artifact_paths(value: Any, repo_root: Path) -> Any:
    if isinstance(value, dict):
        return {
            key: normalize_artifact_paths(child, repo_root)
            for key, child in value.items()
        }
    if isinstance(value, list):
        return [normalize_artifact_paths(child, repo_root) for child in value]
    if isinstance(value, str):
        return normalize_artifact_path_string(value, repo_root)
    return value


def normalize_artifact_path_string(value: str, repo_root: Path) -> str:
    root = os.fspath(repo_root.resolve())
    prefix = root + os.sep
    return value.replace(prefix, "").replace(root, ".")


def refresh_artifact_hashes(result: dict[str, Any]) -> None:
    for case in [*result["cases"], *result["real_repositories"]]:
        for baseline in case.get("baselines", {}).values():
            if "stdout" in baseline:
                baseline["output_hash"] = text_hash(baseline["stdout"])


def write_json(path: Path, value: Any) -> None:
    with path.open("w", encoding="utf-8") as handle:
        json.dump(value, handle, indent=2, sort_keys=True)
        handle.write("\n")


def write_summary(path: Path, result: dict[str, Any]) -> None:
    aggregate = result["aggregate"]
    tools = aggregate["tools"]
    lines = [
        "# Cull Release Benchmark Summary",
        "",
        f"Gate status: **{result['gate']['status']}**",
        "",
        "## Truth-Corpus Metrics",
        "",
        "| Tool | TP | FP | FN | Unmatched | Rule Mismatch | Confidence Mismatch | Precision | Recall | F1 |",
        "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for name in ("cull_high", "cull_reported", "vulture", "deadcode"):
        metric = tools[name]
        lines.append(
            "| {name} | {tp} | {fp} | {fn} | {unmatched} | {rule_mismatch} | {confidence_mismatch} | {precision:.3f} | {recall:.3f} | {f1:.3f} |".format(
                name=name,
                tp=metric["tp"],
                fp=metric["fp"],
                fn=metric["fn"],
                unmatched=metric["unmatched"],
                rule_mismatch=metric["rule_mismatch"],
                confidence_mismatch=metric["confidence_mismatch"],
                precision=metric["precision"],
                recall=metric["recall"],
                f1=metric["f1"],
            )
        )
    lines.extend(["", "## Mode Metrics", ""])
    for mode, mode_aggregate in result["mode_aggregates"].items():
        lines.extend(
            [
                f"### {mode}",
                "",
                "| Tool | TP | FP | FN | Precision | Recall | F1 |",
                "|---|---:|---:|---:|---:|---:|---:|",
            ]
        )
        for name in ("cull_high", "cull_reported", "vulture", "deadcode"):
            metric = mode_aggregate["tools"][name]
            lines.append(
                "| {name} | {tp} | {fp} | {fn} | {precision:.3f} | {recall:.3f} | {f1:.3f} |".format(
                    name=name,
                    tp=metric["tp"],
                    fp=metric["fp"],
                    fn=metric["fn"],
                    precision=metric["precision"],
                    recall=metric["recall"],
                    f1=metric["f1"],
                )
            )
        lines.append("")
    holdout = result["holdout_aggregate"]
    lines.extend(
        [
            "## Holdout Metrics",
            "",
            "| Tool | TP | FP | FN | Precision | Recall | F1 |",
            "|---|---:|---:|---:|---:|---:|---:|",
        ]
    )
    for name in ("cull_high", "cull_reported", "vulture", "deadcode"):
        metric = holdout["tools"][name]
        lines.append(
            "| {name} | {tp} | {fp} | {fn} | {precision:.3f} | {recall:.3f} | {f1:.3f} |".format(
                name=name,
                tp=metric["tp"],
                fp=metric["fp"],
                fn=metric["fn"],
                precision=metric["precision"],
                recall=metric["recall"],
                f1=metric["f1"],
            )
        )
    lines.extend(
        [
            "",
            "## Runtime And Volume",
            "",
            f"- truth cases: {aggregate['case_count']}",
            f"- labeled dead definitions: {aggregate['labeled_dead']}",
            f"- Cull truth-corpus seconds: {aggregate['cull_seconds']:.3f}",
            f"- Cull max case RSS MiB: {aggregate['cull_max_rss_mb']:.1f}",
            "",
            "| Case | Mode | High | Review | Suppressed | Seconds | RSS MiB | Output Hash |",
            "|---|---|---:|---:|---:|---:|---:|---|",
        ]
    )
    for case in result["cases"]:
        summary = case["cull"]["summary"]
        lines.append(
            "| {id} | {mode} | {high} | {review} | {suppressed} | {seconds:.3f} | {rss:.1f} | `{hash}` |".format(
                id=case["id"],
                mode=case["mode"],
                high=summary.get("high_confidence", 0),
                review=summary.get("review", 0),
                suppressed=summary.get("suppressed", 0),
                seconds=case["cull"]["seconds"],
                rss=bytes_to_mb(case["cull"]["max_rss_bytes"] or 0),
                hash=case["cull"]["output_hash"][:12],
            )
        )
    if result["real_repositories"]:
        lines.extend(
            [
                "",
                "## Real Repository Trust Checks",
                "",
                "| Repository | Mode | High | Review | Suppressed | Vulture | deadcode | Seconds | RSS MiB | Output Hash |",
                "|---|---|---:|---:|---:|---:|---:|---:|---:|---|",
            ]
        )
        for case in result["real_repositories"]:
            summary = case["cull"]["summary"]
            baselines = case["baselines"]
            lines.append(
                "| {id} | {mode} | {high} | {review} | {suppressed} | {vulture} | {deadcode} | {seconds:.3f} | {rss:.1f} | `{hash}` |".format(
                    id=case["id"],
                    mode=case["mode"],
                    high=summary.get("high_confidence", 0),
                    review=summary.get("review", 0),
                    suppressed=summary.get("suppressed", 0),
                    vulture=baselines.get("vulture", {}).get("finding_count", 0),
                    deadcode=baselines.get("deadcode", {}).get("finding_count", 0),
                    seconds=case["cull"]["seconds"],
                    rss=bytes_to_mb(case["cull"]["max_rss_bytes"] or 0),
                    hash=case["cull"]["output_hash"][:12],
                )
            )
    lines.extend(["", "## Gate Checks", ""])
    for check in result["gate"]["checks"]:
        status = "pass" if check["pass"] else "fail"
        lines.append(
            f"- {status}: {check['name']} actual={check['actual']} expected={check['expected']}"
        )
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def stable_json_hash(value: Any) -> str:
    normalized = json.dumps(value, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(normalized.encode("utf-8")).hexdigest()


def text_hash(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8")).hexdigest()


def key_to_string(key: FindingKey) -> str:
    return f"{key.path}:{key.line}:{key.kind}:{key.name}"


def ratio(numerator: int, denominator: int) -> float:
    return 0.0 if denominator == 0 else numerator / denominator


def f1(tp: int, fp: int, fn: int) -> float:
    precision = ratio(tp, tp + fp)
    recall = ratio(tp, tp + fn)
    return 0.0 if precision + recall == 0 else 2 * precision * recall / (precision + recall)


def bytes_to_mb(value: int | None) -> float | None:
    if value is None:
        return None
    return value / 1_000_000


if __name__ == "__main__":
    raise SystemExit(main())
