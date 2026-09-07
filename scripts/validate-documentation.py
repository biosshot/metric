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
    cargo_lock = tomllib.loads(read("Cargo.lock"))
    workspace_names = {
        tomllib.loads(read(f"{member}/Cargo.toml"))["package"]["name"]
        for member in cargo["workspace"]["members"]
    }
    for package in cargo_lock["package"]:
        if package["name"] in workspace_names and "source" not in package:
            if package["version"] != version:
                errors.append(f"Cargo.lock: {package['name']} must use version {version}")
    web = json.loads(read("web/package.json"))
    web_lock = json.loads(read("web/package-lock.json"))
    if any(
        item["version"] != version
        for item in (web, web_lock, web_lock["packages"][""])
    ):
        errors.append(f"Web package and lockfile must use version {version}")

    release_notes = f"docs/releases/{version}.md"
    if not (ROOT / release_notes).is_file():
        errors.append(f"Missing release notes: {release_notes}")
    elif not read(release_notes).startswith(f"# Metric {version}\n"):
        errors.append(f"{release_notes}: must start with the matching release heading")

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
        "deploy/profiles/min.env.example",
        "deploy/profiles/low.env.example",
        "deploy/profiles/medium.env.example",
        "deploy/profiles/high.env.example",
        "deploy/compose.yml",
        "deploy/install.ps1",
        "deploy/install.sh",
        "docs/docker.md",
        "docs/configuration.md",
        "docs/getting-started.md",
        "docs/index.md",
        "docs/known-limits.md",
        "docs/kubernetes.md",
        "docs/upgrading.md",
        "charts/metric/README.md",
    )
    release_reference = re.compile(
        r"(?:ghcr\.io/biosshot/metric:|"
        r"(?:raw\.githubusercontent\.com|github\.com)/biosshot/metric/(?:blob/)?v)"
        r"(\d+\.\d+\.\d+)"
    )
    for relative in version_documents:
        contents = read(relative)
        if version not in contents:
            errors.append(f"{relative}: must name the current release version {version}")
        for referenced in release_reference.findall(contents):
            if referenced != version:
                errors.append(
                    f"{relative}: installation reference uses {referenced}, expected {version}"
                )

    if f"/releases/{version}" not in read("docs/.vitepress/config.mts"):
        errors.append("Documentation navigation must link to the current release notes")

    symbolicator_image_documents = (
        "deploy/.env.example",
        "deploy/profiles/min.env.example",
        "deploy/profiles/low.env.example",
        "deploy/profiles/medium.env.example",
        "deploy/profiles/high.env.example",
        "deploy/compose.yml",
        "docs/docker.md",
        "docs/kubernetes.md",
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
    for relative in (
        "deploy/metric.toml",
        "deploy/profiles/medium.toml",
        "deploy/profiles/high.toml",
    ):
        if symbolicator_endpoint not in read(relative):
            errors.append(
                f"{relative}: must use the tested Symbolicator endpoint "
                f"{symbolicator_endpoint}"
            )

    profile_documents = (
        "README.md",
        "docs/capacity.md",
        "docs/configuration.md",
        "docs/docker.md",
        "docs/getting-started.md",
    )
    for relative in profile_documents:
        contents = read(relative)
        for profile in ("Min", "Low", "Medium", "High"):
            if profile not in contents:
                errors.append(f"{relative}: must explain the {profile} profile")

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

    installer_contract = {
        "deploy/install.sh": (
            'install_dir="."',
            "docker volume inspect metric_mongo-data",
            "The installer will not generate a different password for existing data.",
        ),
        "deploy/install.ps1": (
            "(Test-Path -LiteralPath './compose.yml')",
            "docker volume inspect metric_mongo-data",
            "The installer will not generate a different password",
        ),
    }
    for relative, required_values in installer_contract.items():
        contents = read(relative)
        for required in required_values:
            if required not in contents:
                errors.append(
                    f"{relative}: missing repeat-install safety contract: {required}"
                )

    for relative in ("docs/getting-started.md", "docs/operations.md"):
        contents = read(relative)
        if (
            ("volume exists" not in contents and "data exists" not in contents)
            or "generat" not in contents
        ):
            errors.append(
                f"{relative}: must explain password preservation for existing data"
            )

    operator_surface = "\n".join(
        read(relative)
        for relative in (
            "README.md",
            "docs/configuration.md",
            "docs/known-limits.md",
            "docs/index.md",
            "docs/getting-started.md",
            "docs/operations.md",
            "docs/troubleshooting.md",
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
        r"default Compose setup starts Symbolicator automatically",
        r"Remote sign-in over plain HTTP is not supported",
        r"Secure login cookies are not sent over ordinary remote HTTP",
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
    project_markdown.extend(
        path
        for path in (ROOT / "docs").rglob("*.md")
        if "node_modules" not in path.parts
    )
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
