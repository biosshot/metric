#!/usr/bin/env python3
"""Fail closed when compatibility claims lack executable evidence."""

from pathlib import Path
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[1]
MATRIX = ROOT / "compatibility" / "sentry-sdk-matrix.toml"
REQUIRED_FAMILIES = {
    "javascript_browser",
    "javascript_node",
    "python",
    "java",
    "kotlin_android",
    "dotnet",
    "go",
    "rust",
    "php",
    "ruby",
    "cocoa",
    "react_native",
    "flutter_dart",
    "native_cpp",
}
STATES = {"pass", "untested", "disabled", "failed"}


def fail(message: str) -> None:
    raise SystemExit(f"compatibility matrix invalid: {message}")


matrix = tomllib.loads(MATRIX.read_text(encoding="utf-8"))
if matrix.get("manifest_version") != 2:
    fail("manifest_version must be 2")

rows = matrix.get("sdk", [])
families = [row.get("family", row.get("name")) for row in rows]
if len(families) != len(set(families)):
    fail("SDK family rows must be unique")
missing = REQUIRED_FAMILIES.difference(families)
extra = set(families).difference(REQUIRED_FAMILIES)
if missing or extra:
    fail(f"family inventory differs: missing={sorted(missing)}, extra={sorted(extra)}")

for row in rows:
    family = row["family"]
    status = row.get("status")
    if status not in STATES:
        fail(f"{family} has unsupported status {status!r}")
    if row.get("transactions") != "disabled":
        fail(f"{family} must not claim Phase 22 transaction support")
    if status == "pass":
        for field in (
            "version",
            "runtime",
            "error_event",
            "evidence_kind",
            "fixture_set",
            "test",
            "last_passing_server_build",
        ):
            if row.get(field) in (None, "", "none", "unselected", "untested"):
                fail(f"{family} passing row lacks {field}")
        if row["error_event"] != "pass":
            fail(f"{family} passing row does not pass Error Events")
    elif row.get("error_event") == "pass":
        fail(f"{family} claims Error Event support without passing status")

for row in matrix.get("cli", []):
    if row.get("status") == "pass":
        for field in ("version", "evidence_kind", "test", "last_passing_server_build"):
            if row.get(field) in (None, "", "none", "unselected"):
                fail(f"{row.get('name', 'CLI')} passing row lacks {field}")

passed = sorted(row["family"] for row in rows if row["status"] == "pass")
untested = sorted(row["family"] for row in rows if row["status"] == "untested")
print(f"compatibility matrix valid: pass={passed}; untested={untested}")
if "--require-all" in sys.argv and untested:
    fail(f"release gate still has untested SDK families: {untested}")
