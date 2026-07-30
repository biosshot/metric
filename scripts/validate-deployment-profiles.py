#!/usr/bin/env python3
"""Fail when Docker deployment profiles drift from their resource contract."""

from __future__ import annotations

import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
PROFILES = ("min", "low", "medium", "high")
RAM_MIB = {"min": 1024, "low": 2048, "medium": 8192, "high": 16384}
SYMBOLICATION = {"min": False, "low": False, "medium": True, "high": True}


def read(relative: str) -> str:
    return (ROOT / relative).read_text(encoding="utf-8")


def parse_env(relative: str) -> dict[str, str]:
    values: dict[str, str] = {}
    for raw_line in read(relative).splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        key, separator, value = line.partition("=")
        if not separator or not key:
            raise ValueError(f"{relative}: invalid environment line: {raw_line}")
        values[key] = value
    return values


def memory_mib(value: str) -> int:
    normalized = value.strip().lower()
    if normalized.endswith("g"):
        return int(normalized[:-1]) * 1024
    if normalized.endswith("m"):
        return int(normalized[:-1])
    raise ValueError(f"unsupported memory value: {value}")


def main() -> int:
    errors: list[str] = []
    configs: dict[str, dict] = {}
    environments: dict[str, dict[str, str]] = {}

    for profile in PROFILES:
        config_path = f"deploy/profiles/{profile}.toml"
        env_path = f"deploy/profiles/{profile}.env.example"
        configs[profile] = tomllib.loads(read(config_path))
        environments[profile] = parse_env(env_path)

        env = environments[profile]
        config = configs[profile]
        if env.get("METRIC_PROFILE") != profile:
            errors.append(f"{env_path}: METRIC_PROFILE must be {profile}")

        symbols_enabled = "symbolication" in env.get("COMPOSE_PROFILES", "").split(",")
        if symbols_enabled != SYMBOLICATION[profile]:
            errors.append(f"{env_path}: Symbolicator profile selection is incorrect")
        endpoint_present = "endpoint" in config["symbolicator"]
        if endpoint_present != SYMBOLICATION[profile]:
            errors.append(f"{config_path}: Symbolicator endpoint selection is incorrect")

        if config["archive"]["enabled"]:
            errors.append(f"{config_path}: cold archive must remain an explicit operator choice")
        if config["native_crash"]["minidump"]["enabled"]:
            errors.append(f"{config_path}: minidumps must remain an explicit privacy choice")
        if config["ingest"]["attachments"]["enabled"] != (profile != "min"):
            errors.append(f"{config_path}: attachment policy is incorrect")

        active_memory = memory_mib(env["METRIC_MONGO_MEMORY_LIMIT"]) + memory_mib(
            env["METRIC_APP_MEMORY_LIMIT"]
        )
        if SYMBOLICATION[profile]:
            active_memory += memory_mib(env["METRIC_SYMBOLICATOR_MEMORY_LIMIT"])
            active_memory += memory_mib(env["METRIC_CLEANUP_MEMORY_LIMIT"])
        maximum_profile_memory = RAM_MIB[profile] * 9 // 10
        if active_memory > maximum_profile_memory:
            errors.append(
                f"{env_path}: active container limits leave less than 10% host headroom "
                f"({active_memory} MiB > {maximum_profile_memory} MiB)"
            )

    if tomllib.loads(read("deploy/metric.toml")) != configs["medium"]:
        errors.append("deploy/metric.toml must match the Medium profile")
    if parse_env("deploy/.env.example") != environments["medium"]:
        errors.append("deploy/.env.example must match the Medium profile environment")

    monotonic_paths = (
        ("server", "max_active_requests"),
        ("ingest", "max_active_requests"),
        ("ingest", "max_waiting_for_storage"),
        ("dispatcher", "queue_capacity"),
        ("dispatcher", "worker_concurrency"),
        ("processor", "max_concurrency"),
        ("retention", "events_days"),
        ("retention", "feedback_days"),
        ("retention", "issue_stats_hourly_days"),
        ("retention", "logs_days"),
        ("retention", "spans_days"),
        ("retention", "span_stats_hourly_days"),
        ("retention", "sessions_days"),
        ("retention", "session_stats_hourly_days"),
        ("retention", "monitor_runs_days"),
        ("retention", "metrics_days"),
        ("retention", "replays_days"),
    )
    for section, key in monotonic_paths:
        values = [configs[profile][section][key] for profile in PROFILES]
        if values != sorted(values):
            errors.append(
                f"deployment profiles: {section}.{key} must not decrease: {values}"
            )

    capacity_doc = read("docs/capacity.md")
    retention_rows = {
        "Error events": "events_days",
        "Logs": "logs_days",
        "Spans": "spans_days",
        "Hourly span statistics": "span_stats_hourly_days",
        "Feedback": "feedback_days",
        "Hourly issue statistics": "issue_stats_hourly_days",
        "Individual release sessions": "sessions_days",
        "Hourly release statistics": "session_stats_hourly_days",
        "Monitor runs": "monitor_runs_days",
        "Application metrics": "metrics_days",
        "Session Replay, when enabled": "replays_days",
    }
    for label, key in retention_rows.items():
        values = [configs[profile]["retention"][key] for profile in PROFILES]
        expected = f"| {label} | " + " | ".join(
            f"{value} {'day' if value == 1 else 'days'}" for value in values
        ) + " |"
        if expected not in capacity_doc:
            errors.append(f"docs/capacity.md: retention row is stale: {expected}")

    notification_rows = {
        "Delivered notification history": "delivered_days",
        "Failed notification history": "dead_days",
    }
    for label, key in notification_rows.items():
        values = [
            configs[profile]["notifications"]["retention"][key]
            for profile in PROFILES
        ]
        expected = f"| {label} | " + " | ".join(
            f"{value} {'day' if value == 1 else 'days'}" for value in values
        ) + " |"
        if expected not in capacity_doc:
            errors.append(f"docs/capacity.md: retention row is stale: {expected}")

    compose = read("deploy/compose.yml")
    for required in (
        'profiles: ["symbolication"]',
        "required: false",
        "--wiredTigerCacheSizeGB",
        "METRIC_MONGO_MEMORY_LIMIT",
        "METRIC_APP_MEMORY_LIMIT",
        "METRIC_LOG_MAX_SIZE",
    ):
        if required not in compose:
            errors.append(f"deploy/compose.yml: missing profile contract: {required}")

    if errors:
        print("deployment profile validation failed:")
        for error in errors:
            print(f"- {error}")
        return 1

    print(
        "deployment profiles valid: min/low exclude Symbolicator, medium/high "
        "include it, and resource/retention limits scale monotonically"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
