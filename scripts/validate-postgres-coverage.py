#!/usr/bin/env python3
"""Inventory every current database capability method, including port defaults.

This is a source completeness gate, not a substitute for Docker conformance tests.
Until implementation is complete, use --report. Delivery must use --require-complete.
"""

from __future__ import annotations

import argparse
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CAPABILITIES = (
    "ProjectStore", "ProjectDeletionStore", "AuthStore", "EventStore",
    "EventBacklog", "ProcessingProjectStore", "ProcessingStateStore",
    "BlobReferenceStore", "IssueStore", "FinalizationStore", "InvestigationStore",
    "ExploreStore", "DashboardStore", "ReleaseStore", "SignalStore", "MetricStore",
    "SessionStore", "ReplayStore", "MonitorStore", "FeedbackStore", "FeedbackSink",
    "NotificationStore", "ArchiveStore", "MaintenanceStore", "DebugFileStore",
    "ArtifactStore",
)


def code_only(source: str) -> str:
    # Preserve positions/newlines while removing quoted strings and comments.
    pattern = re.compile(r'"(?:\\.|[^"\\])*"|//[^\n]*|/\*.*?\*/', re.DOTALL)
    return pattern.sub(lambda match: re.sub(r"[^\n]", " ", match[0]), source)


def body_at(source: str, offset: int) -> str:
    start = source.index("{", offset)
    depth = 1
    for end in range(start + 1, len(source)):
        depth += (source[end] == "{") - (source[end] == "}")
        if depth == 0:
            return source[start + 1:end]
    raise ValueError("unbalanced Rust capability body")


def methods(body: str) -> set[str]:
    return set(re.findall(r"\bfn\s+(\w+)\s*(?:<|\()", body))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", action="store_true")
    parser.add_argument("--require-complete", action="store_true")
    args = parser.parse_args()
    ports = code_only((ROOT / "crates/ports/src/lib.rs").read_text(encoding="utf-8"))
    implementations: dict[str, set[str]] = {name: set() for name in CAPABILITIES}
    files: dict[str, set[str]] = {name: set() for name in CAPABILITIES}
    for path in sorted((ROOT / "crates/postgres/src").rglob("*.rs")):
        source = code_only(path.read_text(encoding="utf-8"))
        for name in CAPABILITIES:
            for match in re.finditer(rf"\bimpl\s+{name}\s+for\s+\w+", source):
                implementations[name].update(methods(body_at(source, match.end())))
                files[name].add(path.relative_to(ROOT).as_posix())
    rows: list[str] = []
    total = complete = 0
    for name in CAPABILITIES:
        match = re.search(rf"\bpub\s+trait\s+{name}\b", ports)
        if match is None:
            raise ValueError(f"missing expected database port {name}")
        for method in sorted(methods(body_at(ports, match.end()))):
            total += 1
            found = method in implementations[name]
            complete += int(found)
            implementation = ", ".join(f"`{path}`" for path in sorted(files[name])) if found else "Pending"
            rows.append(f"| `{name}::{method}` | {implementation} |")
    if args.report:
        target = ROOT / "arch-docs/module-contracts/0052-postgresql-capability-inventory.md"
        target.write_text(
            "# PostgreSQL capability inventory\n\n"
            "Generated from current ports and explicit adapter implementations by\n"
            "`python scripts/validate-postgres-coverage.py --report`. Port defaults do\n"
            "not count as implementations. Presence is not behavioral verification;\n"
            "see the phase report for actual Docker evidence. Blob bytes remain in\n"
            "the existing shared Blob adapter.\n\n"
            f"Implemented signatures: {complete}/{total}. Delivery requires every row\n"
            "and its conformance evidence, including signatures added after this snapshot.\n\n"
            "| Port method | Explicit implementation |\n| --- | --- |\n"
            + "\n".join(rows) + "\n",
            encoding="utf-8",
        )
    print(f"PostgreSQL capability signatures: {complete}/{total}; pending: {total-complete}")
    return int(args.require_complete and complete != total)


if __name__ == "__main__":
    raise SystemExit(main())
