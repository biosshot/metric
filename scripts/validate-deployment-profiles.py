#!/usr/bin/env python3
"""Fail when Docker deployment profiles drift from their resource contract."""

from __future__ import annotations

import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
PROFILES = ("min", "low", "medium", "high")
RAM_MIB = {"min": 1024, "low": 2048, "medium": 8192, "high": 16384}
DISK_GIB = {"min": 15, "low": 30, "medium": 100, "high": 250}
SYMBOLICATION = {"min": False, "low": False, "medium": True, "high": True}
INGEST_ACTIVE = {"min": 64, "low": 256, "medium": 1024, "high": 4096}
PARSING_TASKS = {"min": 2, "low": 4, "medium": 8, "high": 16}
STORAGE_QUEUE = {"min": 128, "low": 512, "medium": 2048, "high": 8192}
BATCH_DOCUMENTS = {"min": 128, "low": 250, "medium": 500, "high": 500}
BATCH_MIB = {"min": 2, "low": 8, "medium": 32, "high": 64}


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


def storage_mib(value: str) -> int:
    number, unit = value.strip().split()
    amount = int(number)
    if unit == "GiB":
        return amount * 1024
    if unit == "MiB":
        return amount
    raise ValueError(f"unsupported storage value: {value}")


def retention_label(days: int) -> str:
    if days % 365 == 0:
        years = days // 365
        return f"{years} {'year' if years == 1 else 'years'}"
    return f"{days} {'day' if days == 1 else 'days'}"


def nested(config: dict, path: tuple[str, ...]):
    value = config
    for key in path:
        value = value[key]
    return value


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
        if config.get("development", {}).get("allow_insecure_cookies") is not True:
            errors.append(
                f"{config_path}: supplied profiles must allow sign-in over HTTP"
            )
        if config.get("auth", {}).get("secure_cookie") is not False:
            errors.append(
                f"{config_path}: supplied profiles must send the login cookie over HTTP"
            )
        ingest = config["ingest"]
        if ingest["max_active_requests"] != INGEST_ACTIVE[profile]:
            errors.append(
                f"{config_path}: ingest admission must be {INGEST_ACTIVE[profile]}"
            )
        if ingest["max_parsing_tasks"] != PARSING_TASKS[profile]:
            errors.append(
                f"{config_path}: parsing concurrency must be {PARSING_TASKS[profile]}"
            )
        if ingest["max_waiting_for_storage"] != STORAGE_QUEUE[profile]:
            errors.append(
                f"{config_path}: storage queue must be {STORAGE_QUEUE[profile]}"
            )
        if ingest["max_waiting_for_storage"] < ingest["max_active_requests"] * 2:
            errors.append(
                f"{config_path}: storage queue must absorb at least two full "
                "admission windows"
            )
        if ingest["batch"]["max_documents"] != BATCH_DOCUMENTS[profile]:
            errors.append(
                f"{config_path}: batch document limit must be "
                f"{BATCH_DOCUMENTS[profile]}"
            )
        if storage_mib(ingest["batch"]["max_bytes"]) != BATCH_MIB[profile]:
            errors.append(
                f"{config_path}: batch byte limit must be {BATCH_MIB[profile]} MiB"
            )

        blob_capacity = storage_mib(config["blob"]["capacity"])
        expected_capacity = DISK_GIB[profile] // 3 * 1024
        if blob_capacity != expected_capacity:
            errors.append(
                f"{config_path}: BlobStore must use one third of the recommended "
                f"disk ({expected_capacity // 1024} GiB)"
            )
        blob_reserve = storage_mib(config["blob"]["reserve"])
        if not blob_capacity * 4 // 100 <= blob_reserve <= blob_capacity // 10:
            errors.append(
                f"{config_path}: BlobStore reserve must stay between 4% and 10% "
                "of capacity"
            )
        blob_object = storage_mib(config["blob"]["max_object_bytes"])
        object_consumers = (
            storage_mib(config["artifacts"]["maximum_bundle_bytes"]),
            storage_mib(config["incident_capsule"]["max_total_uncompressed_bytes"]),
            storage_mib(config["ingest"]["attachments"]["max_total_bytes"]),
            storage_mib(config["ingest"]["replay"]["max_segment_bytes"]),
        )
        if any(value > blob_object for value in object_consumers):
            errors.append(
                f"{config_path}: an upload limit exceeds blob.max_object_bytes"
            )
        artifact_quota = storage_mib(
            config["artifacts"]["maximum_bytes_per_organization"]
        )
        if artifact_quota > blob_capacity - blob_reserve:
            errors.append(
                f"{config_path}: one organization artifact quota exceeds writable "
                "BlobStore capacity"
            )

        retention = config["retention"]
        if (
            retention["logs_days"] > retention["events_days"]
            or retention["spans_days"] > retention["events_days"]
            or retention["issue_stats_hourly_days"] < retention["events_days"]
            or retention["span_stats_hourly_days"] < retention["spans_days"]
            or retention["session_stats_hourly_days"] < retention["sessions_days"]
        ):
            errors.append(
                f"{config_path}: compact statistics must outlive high-volume raw data"
            )

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
        ("ingest", "max_envelope_items"),
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

    monotonic_size_paths = (
        ("blob", "capacity"),
        ("blob", "reserve"),
        ("blob", "max_object_bytes"),
        ("artifacts", "maximum_bundle_bytes"),
        ("artifacts", "maximum_logical_bytes"),
        ("artifacts", "maximum_bytes_per_organization"),
        ("incident_capsule", "max_total_uncompressed_bytes"),
        ("ingest", "max_compressed_request_bytes"),
        ("ingest", "max_decompressed_request_bytes"),
        ("ingest", "max_event_bytes"),
        ("ingest", "attachments", "max_item_bytes"),
        ("ingest", "attachments", "max_total_bytes"),
        ("ingest", "replay", "max_segment_bytes"),
        ("ingest", "replay", "max_queued_bytes"),
        ("ingest", "batch", "max_bytes"),
    )
    for path in monotonic_size_paths:
        values = [
            storage_mib(nested(configs[profile], path))
            for profile in PROFILES
        ]
        if values != sorted(values):
            errors.append(
                f"deployment profiles: {'.'.join(path)} must not decrease: {values}"
            )

    capacity_doc = read("docs/capacity.md")
    readme = read("README.md")
    for profile in PROFILES:
        expected_capacity = DISK_GIB[profile] // 3
        expected_cell = f"| **{profile.title()}** | "
        matching_rows = [
            line
            for line in capacity_doc.splitlines()
            if line.startswith(expected_cell)
        ]
        if not matching_rows or f"| {expected_capacity} GiB |" not in matching_rows[0]:
            errors.append(
                f"docs/capacity.md: {profile} BlobStore capacity must be "
                f"{expected_capacity} GiB"
            )

        error_days = configs[profile]["retention"]["events_days"]
        readme_values = {
            "min": ("1 vCPU, 1 GiB RAM, 15 GiB SSD", "No"),
            "low": ("2 vCPU, 2 GiB RAM, 30 GiB SSD", "No"),
            "medium": ("4 vCPU, 8 GiB RAM, 100 GiB SSD", "Yes"),
            "high": ("8 vCPU, 16 GiB RAM, 250 GiB SSD", "Yes"),
        }
        server, symbols = readme_values[profile]
        expected_readme_row = (
            f"| {profile.title()} | {server} | {expected_capacity} GiB | "
            f"{symbols} | {error_days} days |"
        )
        if expected_readme_row not in readme:
            errors.append(f"README.md: deployment row is stale: {expected_readme_row}")

    expected_capacity_summary = (
        "Profiles use 5 GiB, 10 GiB, 33 GiB or 83 GiB."
    )
    configuration_doc = read("docs/configuration.md")
    if expected_capacity_summary not in configuration_doc:
        errors.append(
            "docs/configuration.md: BlobStore profile capacity summary is stale"
        )
    expected_ingest_summary = (
        "Profiles use 64, 256, 1024 or 4096."
    )
    if expected_ingest_summary not in configuration_doc:
        errors.append(
            "docs/configuration.md: ingest concurrency summary is stale"
        )

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
            retention_label(value) for value in values
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
            retention_label(value) for value in values
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
        "include it, HTTP sign-in is enabled, foreground ingest uses aggressive "
        "bounded batching, BlobStore uses one third of disk, and resource/retention "
        "limits scale monotonically"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
