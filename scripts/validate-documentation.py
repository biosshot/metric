#!/usr/bin/env python3
"""Fail when current operator documentation drifts from runtime contracts."""

from __future__ import annotations

import json
import re
import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def read(relative: str) -> str:
    return (ROOT / relative).read_text(encoding="utf-8")


def main() -> int:
    errors: list[str] = []
    cargo = tomllib.loads(read("Cargo.toml"))
    version = cargo["workspace"]["package"]["version"]
    symbolicator_contract = json.loads(
        read("sdk-tests/symbolicator/26.6.0-native-contract.json")
    )
    symbolicator_image = symbolicator_contract["image"]
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

    version_documents = (
        "README.md",
        "deploy/.env.example",
        "deploy/compose.yml",
        "deploy/install.ps1",
        "deploy/install.sh",
        "docs/docker.md",
        "docs/getting-started.md",
        "docs/known-limits.md",
    )
    for relative in version_documents:
        if version not in read(relative):
            errors.append(f"{relative}: must name the current release version {version}")

    symbolicator_image_documents = (
        "deploy/.env.example",
        "deploy/compose.yml",
        "deploy/install.ps1",
        "deploy/install.sh",
        "docs/docker.md",
        "THIRD_PARTY_NOTICES.md",
    )
    for relative in symbolicator_image_documents:
        if symbolicator_image not in read(relative):
            errors.append(
                f"{relative}: must name the tested Symbolicator image {symbolicator_image}"
            )

    symbolicator_config_documents = (
        "deploy/compose.yml",
        "deploy/install.ps1",
        "deploy/install.sh",
        "docs/docker.md",
        "docs/getting-started.md",
        "docs/operations.md",
    )
    for relative in symbolicator_config_documents:
        if "symbolicator.yml" not in read(relative):
            errors.append(
                f"{relative}: must include the deployed symbolicator.yml configuration"
            )

    symbolicator_endpoint = (
        f'endpoint = "http://symbolicator:3021{symbolicator_contract["endpoint"]}"'
    )
    if symbolicator_endpoint not in read("deploy/metric.toml"):
        errors.append(
            "deploy/metric.toml: must use the tested Symbolicator endpoint "
            f"{symbolicator_endpoint}"
        )

    current_deployment_surface = "\n".join(
        read(relative)
        for relative in (
            "README.md",
            "Dockerfile",
            "deploy/compose.yml",
            "docs/configuration.md",
            "docs/docker.md",
            "docs/getting-started.md",
            "docs/operations.md",
            "docs/troubleshooting.md",
            "docs/upgrading.md",
        )
    )
    for obsolete in (
        "compose.release.yml",
        "metric.container.toml",
        "release.env",
    ):
        if obsolete in current_deployment_surface:
            errors.append(f"current deployment documentation contains obsolete path: {obsolete}")

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
        r"runs two containers",
        r"does not include a Symbolicator container",
        r"external Symbolicator is optional and operated separately",
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

    compatibility = read("docs/compatibility.md")
    sdk_guide = read("docs/sdk-setup.md")
    matrix = tomllib.loads(read("compatibility/sentry-sdk-matrix.toml"))
    for sdk in matrix["sdk"]:
        if sdk["status"] != "pass":
            continue
        name = sdk["name"]
        tested_version = sdk["version"]
        if name not in compatibility or tested_version not in compatibility:
            errors.append(
                f"docs/compatibility.md: missing tested SDK {name} {tested_version}"
            )
        if name not in sdk_guide or tested_version not in sdk_guide:
            errors.append(f"docs/sdk-setup.md: missing tested SDK {name} {tested_version}")

    upgrade = read("docs/upgrading.md")
    for required in (
        "never drop or recreate a data-bearing MongoDB database",
        "Never edit the `schema_meta` generation manually",
        "MongoDB and the configured BlobStore as one operational unit",
    ):
        if required not in upgrade:
            errors.append(f"docs/upgrading.md: missing data-safety invariant: {required}")

    project_markdown = [ROOT / "README.md", ROOT / "THIRD_PARTY_NOTICES.md"]
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
        f"documentation validation passed: version {version}, schema generation "
        f"{generation}, Symbolicator {symbolicator_image}, deployment paths and "
        "tested SDK versions agree"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
