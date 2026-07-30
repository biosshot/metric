#!/usr/bin/env python3
"""Fail when current operator documentation drifts from runtime contracts."""

from __future__ import annotations

import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def read(relative: str) -> str:
    return (ROOT / relative).read_text(encoding="utf-8")


def main() -> int:
    errors: list[str] = []
    runtime = read("crates/mongo/src/lib.rs")
    match = re.search(r"pub const SCHEMA_GENERATION: i32 = (\d+);", runtime)
    if match is None:
        print("documentation validation failed: SCHEMA_GENERATION was not found")
        return 1
    generation = int(match.group(1))

    current_documents = (
        "README.md",
        "docs/configuration.md",
        "docs/known-limits.md",
        "docs/operations.md",
        "docs/upgrading.md",
        "arch-docs/README.md",
    )
    generation_claim = re.compile(
        rf"schema generation (?:\*\*)?{generation} exactly(?:\*\*)?",
        re.IGNORECASE,
    )
    for relative in current_documents:
        if generation_claim.search(read(relative)) is None:
            errors.append(
                f"{relative}: must state that schema generation {generation} is required exactly"
            )

    operator_surface = "\n".join(
        read(relative)
        for relative in (
            "README.md",
            "docs/configuration.md",
            "docs/known-limits.md",
            "docs/operations.md",
            "docs/supported-capabilities.md",
            "docs/compatibility.md",
        )
    )
    stale_patterns = (
        r"schema generation (?:7|18) (?:supports|permits|bootstraps)",
        r"Session Replay and Profiling are disabled",
        r"Profiling and Session Replay remain disabled",
        r"Session Replay[^.\n]*(?:not implemented|is next)",
    )
    for pattern in stale_patterns:
        if re.search(pattern, operator_surface, re.IGNORECASE):
            errors.append(f"operator documentation contains stale claim: {pattern}")

    capabilities = read("docs/supported-capabilities.md")
    replay_contract = ("Session Replay", "`@sentry/browser`", "10.66.0")
    if not all(value in capabilities for value in replay_contract):
        errors.append(
            "docs/supported-capabilities.md: must name the tested Session Replay SDK version"
        )

    upgrade = read("docs/upgrading.md")
    for required in (
        "never drop or recreate a data-bearing MongoDB database",
        "Never edit the `schema_meta` generation manually",
        "MongoDB and the configured BlobStore as one operational unit",
    ):
        if required not in upgrade:
            errors.append(f"docs/upgrading.md: missing data-safety invariant: {required}")

    project_markdown = [ROOT / "README.md"]
    project_markdown.extend((ROOT / "docs").rglob("*.md"))
    project_markdown.extend((ROOT / "arch-docs").rglob("*.md"))
    all_markdown = "\n".join(
        path.read_text(encoding="utf-8") for path in project_markdown
    )
    if "must be dropped or recreated by its operator" in all_markdown:
        errors.append(
            "documentation contains the obsolete instruction to drop/recreate an operator database"
        )

    if errors:
        print("documentation validation failed:")
        for error in errors:
            print(f"- {error}")
        return 1

    print(
        f"documentation validation passed: runtime and operator docs agree on "
        f"schema generation {generation}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
