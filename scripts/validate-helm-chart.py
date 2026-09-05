#!/usr/bin/env python3
"""Validate chart versions, deployment invariants and actual Metric configuration."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[1]
CHART = ROOT / "charts/metric"


def run(*command: str) -> str:
    result = subprocess.run(command, cwd=ROOT, text=True, encoding="utf-8", capture_output=True)
    if result.returncode:
        raise RuntimeError(f"{command[0]} failed:\n{result.stdout}\n{result.stderr}")
    return result.stdout


def render(values: dict, folder: Path, expect_error: bool = False) -> list[dict]:
    path = folder / "values.json"
    path.write_text(json.dumps(values), encoding="utf-8")
    command = ["helm", "template", "metric-check", str(CHART), "--namespace", "metric-check", "-f", str(path)]
    if expect_error:
        result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, encoding="utf-8")
        assert result.returncode != 0, f"Invalid chart values were accepted: {values}"
        return []
    return [item for item in yaml.safe_load_all(run(*command)) if item]


def named(docs: list[dict], kind: str, name: str = "metric-check") -> dict:
    return next(d for d in docs if d["kind"] == kind and d["metadata"]["name"] == name)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", help="Local image used to execute --check-config for every scenario")
    parser.add_argument("--release-tag", help="Also require this vMAJOR.MINOR.PATCH release tag")
    args = parser.parse_args()
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    metadata = yaml.safe_load((CHART / "Chart.yaml").read_text())
    assert metadata["version"] == metadata["appVersion"] == version, "Cargo/chart/app versions differ"
    if args.release_tag:
        assert args.release_tag == f"v{version}", "Git release tag does not match Cargo/chart/image"
    print(run(sys.executable, "scripts/sync-helm-profiles.py", "--check").strip())
    print(run("helm", "lint", str(CHART), "--strict").strip())

    scenarios = {
        **{p: {"profile": p} for p in ("min", "low", "medium", "high")},
        "external": {"mongodb": {"enabled": False}, "secrets": {"existingSecret": "external-secrets"}},
        "s3": {"blob": {"backend": "s3", "s3": {"bucket": "metric-test", "endpoint": "http://s3:9000", "sessionToken": True}}},
        "ingress": {"ingress": {"enabled": True, "host": "metric.example.com", "tls": [{"hosts": ["metric.example.com"], "secretName": "metric-tls"}]}, "http": {"secureCookies": True, "trustedProxies": ["10.0.0.0/8"]}},
        "existing-claims": {"persistence": {"existingClaim": "old-blobs"}, "mongodb": {"persistence": {"existingClaim": "old-mongo"}}},
        "external-symbolicator": {"profile": "medium", "symbolicator": {"externalEndpoint": "http://symbols.example.com:3021/symbolicate"}},
        "disabled-symbolicator": {"profile": "high", "symbolicator": {"enabled": False}},
        "overrides": {"config": {"retention": {"events_days": 47}, "server": {"request_timeout": "45s"}}, "persistence": {"size": "8Gi", "storageClass": ""}, "extraEnv": [{"name": "SMTP_PASSWORD", "valueFrom": {"secretKeyRef": {"name": "smtp", "key": "password"}}}]},
    }
    invalid = [
        {"replicaCount": 2}, {"replicaCount": 0}, {"profile": "typo"},
        {"image": {"tag": "999.0.0"}}, {"image": {"tag": "latest"}},
        {"config": {"role": "all"}}, {"config": {"server": {"http_address": "0.0.0.0:5000"}}},
        {"config": {"blob": {"root": "/tmp/lost"}}},
        {"config": {"development": {"allow_literal_secrets": True}}},
        {"config": {"retention": {"events_days": 1.5}}},
        {"extraEnv": [{"name": "APP__ROLE", "value": "all"}]},
        {"extraEnv": [{"name": "SCRUB_HMAC_KEY", "value": "wrong"}]},
        {"extraEnv": [{"name": "FOO", "value": "1"}, {"name": "FOO", "value": "2"}]},
        {"ingress": {"enabled": True}}, {"blob": {"backend": "s3"}},
        {"persistence": {"size": "1Gi"}}, {"terminationGracePeriodSeconds": 10},
        {"podSecurityContext": {"runAsUser": 0}}, {"podAnnotations": {"checksum/config": "override"}},
        {"persistence": {"annotations": {"helm.sh/resource-policy": "delete"}}},
        {"mongodb": {"persistence": {"annotations": {"helm.sh/resource-policy": "delete"}}}},
        {"replicas": 2},
    ]
    with tempfile.TemporaryDirectory(prefix="metric-helm-render-") as temp:
        folder = Path(temp)
        for scenario, values in scenarios.items():
            docs = render(values, folder)
            deployment = named(docs, "Deployment")
            pod = deployment["spec"]["template"]["spec"]
            server = pod["containers"][0]
            assert deployment["spec"]["replicas"] == 1
            assert deployment["spec"]["strategy"] == {"type": "Recreate"}
            assert server["image"] == f"ghcr.io/biosshot/metric:{version}"
            assert not pod["automountServiceAccountToken"]
            assert server["startupProbe"]["httpGet"]["path"] == "/live"
            assert server["livenessProbe"]["httpGet"]["path"] == "/live"
            assert server["readinessProbe"]["httpGet"]["path"] == "/ready"
            assert all(d["kind"] != "Secret" for d in docs), "Chart must not own installation credentials"
            for pvc in (d for d in docs if d["kind"] == "PersistentVolumeClaim"):
                assert pvc["metadata"]["annotations"]["helm.sh/resource-policy"] == "keep"
            config_text = named(docs, "ConfigMap")["data"]["metric.toml"]
            config = tomllib.loads(config_text)
            assert config["role"] == "all"
            assert config["server"]["http_address"] == "0.0.0.0:4001"
            assert config["projects"]["scrub_hmac_key"] == {"env": "SCRUB_HMAC_KEY"}
            profile = values.get("profile", "min")
            expected = tomllib.loads((ROOT / f"deploy/profiles/{profile}.toml").read_text())
            assert type(config["server"]["max_active_requests"]) is int, "TOML integer became float"
            assert config["ingest"] == expected["ingest"], "Chart diverged from profile admission limits"
            if scenario == "s3":
                assert config["blob"]["s3"]["session_token"] == {"env": "S3_SESSION_TOKEN"}
                assert not any(v["name"] == "blobs" for v in pod["volumes"])
            if scenario == "external":
                assert all(d["kind"] != "StatefulSet" for d in docs)
                assert server["env"][0]["valueFrom"]["secretKeyRef"]["key"] == "mongodb-uri"
            if scenario == "existing-claims":
                assert not any(d["kind"] == "PersistentVolumeClaim" for d in docs)
            if scenario == "ingress":
                assert named(docs, "Ingress")["spec"]["rules"][0]["host"] == "metric.example.com"
                assert config["auth"]["secure_cookie"] is True
                assert config["development"]["allow_insecure_cookies"] is False
            if scenario == "overrides":
                assert config["retention"]["events_days"] == 47
                assert named(docs, "PersistentVolumeClaim", "metric-check-blobs")["spec"]["storageClassName"] == ""
            if scenario == "disabled-symbolicator":
                assert "endpoint" not in config["symbolicator"]
            if scenario == "external-symbolicator":
                assert not any(d["metadata"]["name"] == "metric-check-symbolicator" for d in docs)
                assert config["symbolicator"]["endpoint"].startswith("http://symbols.example.com")
            if args.image:
                config_path = folder / "metric.toml"
                config_path.write_text(config_text, encoding="utf-8")
                command = ["docker", "run", "--rm", "--network", "none", "--mount", f"type=bind,src={config_path},dst=/tmp/helm-config.toml,readonly", "--env", "MONGODB_URI=mongodb://example.invalid:27017", "--env", "SCRUB_HMAC_KEY=" + "ab" * 32, "--env", "S3_ACCESS_KEY_ID=fixture", "--env", "S3_SECRET_ACCESS_KEY=fixture", "--env", "S3_SESSION_TOKEN=fixture", args.image, "--config", "/tmp/helm-config.toml", "--check-config"]
                run(*command)
            print(f"PASS {scenario}", flush=True)
        for values in invalid:
            render(values, folder, expect_error=True)
        # Config changes restart the pod; equivalent renders stay deterministic.
        baseline = named(render({}, folder), "Deployment")["spec"]["template"]["metadata"]["annotations"]
        repeat = named(render({}, folder), "Deployment")["spec"]["template"]["metadata"]["annotations"]
        changed = named(render({"config": {"retention": {"events_days": 61}}}, folder), "Deployment")["spec"]["template"]["metadata"]["annotations"]
        assert baseline == repeat and baseline["checksum/config"] != changed["checksum/config"]
    print(f"Helm validated: version {version}, {len(scenarios)} configurations, {len(invalid)} rejected cases.")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (AssertionError, RuntimeError) as error:
        print(f"Helm validation failed: {error}", file=sys.stderr)
        sys.exit(1)
