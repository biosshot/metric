#!/usr/bin/env python3
"""Publish the matching chart without overwriting a different existing release."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tarfile
import tempfile
import tomllib
from pathlib import Path
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parents[1]


def run(command: list[str], env: dict | None = None) -> str:
    result = subprocess.run(command, cwd=ROOT, env=env, capture_output=True, text=True, encoding="utf-8", timeout=180)
    if result.returncode:
        raise RuntimeError(result.stderr.strip())
    return result.stdout


def archive_contents(path: Path) -> dict[str, bytes]:
    # Compare contents rather than gzip/tar timestamps; no extraction is needed.
    with tarfile.open(path, "r:gz") as archive:
        return {entry.name: archive.extractfile(entry).read() for entry in archive if entry.isfile()}


def publish(package: Path, registry: str, version: str, folder: Path) -> None:
    remote = registry.rstrip("/") + "/metric"
    # The CI fixture registry is loopback-only HTTP; remote registries always use TLS.
    transport = ["--plain-http"] if urlsplit(registry).hostname in ("localhost", "127.0.0.1", "::1") else []
    prior = folder / "prior"
    prior.mkdir()
    probe = subprocess.run(
        ["helm", "pull", remote, "--version", version, "--destination", str(prior), *transport],
        capture_output=True, text=True, encoding="utf-8", timeout=180,
    )
    if probe.returncode == 0:
        if archive_contents(package) != archive_contents(prior / package.name):
            raise RuntimeError(f"{remote}:{version} already exists with different content; publish the next Metric version")
        print(f"Identical chart {version} already published; not overwriting.")
    else:
        # A registry permission/network error must never be treated as absence.
        diagnostic = probe.stderr.lower()
        missing_tag = remote.removeprefix("oci://").lower() + f":{version}: not found"
        missing_manifest = f"/manifests/{version}" in diagnostic and "404" in diagnostic
        if not (missing_tag in diagnostic or missing_manifest or "manifest_unknown" in diagnostic or "name_unknown" in diagnostic):
            raise RuntimeError(
                "Could not check whether the chart already exists: " + probe.stderr.strip()
                + ". Check package permissions. For an absent first-ever package, "
                "follow the one-time bootstrap procedure in docs/kubernetes.md."
            )
        print(run(["helm", "push", str(package), registry, *transport]), end="")
    anonymous_folder = folder / "anonymous"
    anonymous_folder.mkdir()
    anonymous = os.environ.copy()
    anonymous["HELM_REGISTRY_CONFIG"] = str(anonymous_folder / "registry.json")
    # Helm can also fall back to the Docker credential store. Isolate both.
    anonymous["DOCKER_CONFIG"] = str(anonymous_folder)
    # Empty auths enables native-store auto-detection (e.g. Windows wincred).
    # An explicit empty registry entry disables that lookup without using a secret.
    anonymous_config = json.dumps({"auths": {urlsplit(registry).netloc: {}}})
    (anonymous_folder / "registry.json").write_text(anonymous_config, encoding="utf-8")
    (anonymous_folder / "config.json").write_text(anonymous_config, encoding="utf-8")
    try:
        run(["helm", "pull", remote, "--version", version, "--destination", str(anonymous_folder), *transport], env=anonymous)
    except RuntimeError as error:
        raise RuntimeError("Chart was published but anonymous pull failed. Make its GitHub package public, grant this repository Actions access, then rerun this job. " + str(error)) from error
    if archive_contents(package) != archive_contents(anonymous_folder / package.name):
        raise RuntimeError("Anonymously downloaded chart differs from the release package")
    print(f"Published and anonymously verified {remote}:{version}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--registry", required=True, help="OCI parent, e.g. oci://ghcr.io/biosshot/charts")
    parser.add_argument("--release-tag", required=True)
    args = parser.parse_args()
    run([sys.executable, "scripts/validate-helm-chart.py", "--release-tag", args.release_tag])
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    if not args.registry.startswith("oci://"):
        parser.error("--registry must be an OCI parent URL")
    with tempfile.TemporaryDirectory(prefix="metric-chart-publish-") as temp:
        folder = Path(temp)
        package = folder / f"metric-{version}.tgz"
        run(["helm", "package", "charts/metric", "--destination", temp])
        publish(package, args.registry, version, folder)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (RuntimeError, subprocess.TimeoutExpired) as error:
        print(f"Chart publication failed: {error}", file=sys.stderr)
        sys.exit(1)
