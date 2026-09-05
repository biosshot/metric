#!/usr/bin/env python3
"""Generate the chart's profile data from the existing Compose contracts."""

from __future__ import annotations

import argparse
import json
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
TARGET = ROOT / "charts/metric/files/profiles.json"


def generate() -> str:
    profiles = {}
    for name in ("min", "low", "medium", "high"):
        source = ROOT / "deploy/profiles"
        config = tomllib.loads((source / f"{name}.toml").read_text(encoding="utf-8"))
        env = dict(
            line.split("=", 1)
            for line in (source / f"{name}.env.example").read_text(encoding="utf-8").splitlines()
            if line and not line.startswith("#")
        )

        def resource(key: str) -> dict:
            value = env[key].lower()
            mib = int(value[:-1]) * (1024 if value.endswith("g") else 1)
            return {
                "requests": {"cpu": "100m", "memory": f"{mib // 2}Mi"},
                "limits": {"memory": f"{mib}Mi"},
            }

        profiles[name] = {
            "config": config,
            "resources": resource("METRIC_APP_MEMORY_LIMIT"),
            "mongoResources": resource("METRIC_MONGO_MEMORY_LIMIT"),
            "mongoCacheSizeGB": env["METRIC_MONGO_CACHE_GB"],
            "symbolicatorEnabled": "symbolication" in env["COMPOSE_PROFILES"],
            "symbolicatorResources": resource("METRIC_SYMBOLICATOR_MEMORY_LIMIT"),
            "cleanupResources": resource("METRIC_CLEANUP_MEMORY_LIMIT"),
        }
    # JSON is also YAML. The config renderer restores integers before toToml.
    return json.dumps(profiles, indent=2, sort_keys=True) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    expected = generate()
    if args.check:
        if not TARGET.exists() or TARGET.read_text(encoding="utf-8") != expected:
            print("Helm profiles are stale. Run python scripts/sync-helm-profiles.py.")
            return 1
        print("Helm profiles match the Compose configurations and resource limits.")
    else:
        TARGET.parent.mkdir(parents=True, exist_ok=True)
        TARGET.write_text(expected, encoding="utf-8", newline="\n")
        print("Generated charts/metric/files/profiles.json")
    return 0


if __name__ == "__main__":
    sys.exit(main())
