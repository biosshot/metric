#!/usr/bin/env python3
"""Exercise the chart in an explicitly selected disposable Kind cluster."""

from __future__ import annotations

import argparse
import http.cookiejar
import json
import queue
import re
import secrets
import subprocess
import sys
import tempfile
import threading
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CHART = ROOT / "charts/metric"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--context", required=True)
    parser.add_argument("--kubeconfig")
    parser.add_argument("--image-repository", required=True)
    parser.add_argument("--keep-on-failure", action="store_true")
    parser.add_argument("--with-symbolicator", action="store_true", help="Also exercise the pinned Symbolicator and cleanup containers")
    args = parser.parse_args()
    if not args.context.startswith("kind-"):
        parser.error("Only an explicit disposable kind- context is accepted")
    namespace = "metric-helm-test-" + secrets.token_hex(4)
    kube = ["kubectl", "--context", args.context]
    helm = ["helm", "--kube-context", args.context]
    if args.kubeconfig:
        kube += ["--kubeconfig", args.kubeconfig]
        helm += ["--kubeconfig", args.kubeconfig]
    scoped = [*kube, "--namespace", namespace]
    helm += ["--namespace", namespace]

    def execute(command: list[str], data: str | None = None, timeout: int = 300) -> str:
        result = subprocess.run(command, input=data, capture_output=True, text=True, encoding="utf-8", timeout=timeout)
        if result.returncode:
            # Commands and input can contain fixture credentials; print only diagnostics.
            raise RuntimeError(f"{command[0]} failed: {result.stderr.strip()}")
        return result.stdout

    def get(resource: str, name: str | None = None) -> dict:
        return json.loads(execute([*scoped, "get", resource, *([name] if name else []), "-o", "json"]))

    def pod_name() -> str:
        pods = get("pods")["items"]
        active = [p for p in pods if p["metadata"].get("labels", {}).get("app.kubernetes.io/component") == "server" and "deletionTimestamp" not in p["metadata"]]
        assert len(active) == 1, "Expected one active Metric pod"
        return active[0]["metadata"]["name"]

    forward: subprocess.Popen | None = None
    created = False
    success = False
    try:
        execute([*kube, "create", "namespace", namespace])
        created = True
        execute([*scoped, "create", "-f", "-"], json.dumps({
            "apiVersion": "v1", "kind": "Secret", "metadata": {"name": "metric-secrets"},
            "stringData": {"mongo-password": secrets.token_hex(24), "scrub-hmac-key": secrets.token_hex(32)},
        }))
        schema_scenarios = [
            ["--set", f"profile={profile}"] for profile in ("min", "low", "medium", "high")
        ] + [
            ["--set", "mongodb.enabled=false"],
            ["--set", "blob.backend=s3,blob.s3.bucket=metric-test"],
            ["--set", "ingress.enabled=true,ingress.host=metric.example.com,http.secureCookies=true"],
        ]
        for options in schema_scenarios:
            manifests = execute([*helm, "template", "metric-schema-check", str(CHART), *options])
            execute([*scoped, "create", "--dry-run=server", "--validate=strict", "-f", "-"], manifests)
        print("PASS Kubernetes API validation: profiles, external MongoDB, S3 and Ingress", flush=True)
        with tempfile.TemporaryDirectory(prefix="metric-helm-cluster-") as temp:
            values_path = Path(temp) / "values.json"
            values = {"image": {"repository": args.image_repository, "pullPolicy": "Never"}, "profile": "min"}
            if args.with_symbolicator:
                values["symbolicator"] = {"enabled": True}
            values_path.write_text(json.dumps(values), encoding="utf-8")

            def install() -> None:
                execute([*helm, "install", "metric", str(CHART), "-f", str(values_path), "--wait", "--timeout", "5m"], timeout=330)

            def http_session() -> tuple[urllib.request.OpenerDirector, str]:
                nonlocal forward
                if forward is not None:
                    forward.terminate()
                    forward.wait(timeout=10)
                # Let kubectl select an unused local port, bound to loopback only.
                forward = subprocess.Popen([*scoped, "port-forward", "service/metric", ":4001", "--address", "127.0.0.1"], stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, encoding="utf-8")
                assert forward.stdout is not None
                lines: queue.Queue[str] = queue.Queue(maxsize=1)
                threading.Thread(target=lambda: lines.put(forward.stdout.readline()), daemon=True).start()
                try:
                    line = lines.get(timeout=30)
                except queue.Empty as error:
                    raise RuntimeError("Timed out establishing the isolated service port-forward") from error
                match = re.search(r"127\.0\.0\.1:(\d+)", line)
                if match is None:
                    raise RuntimeError("Could not establish the isolated service port-forward: " + line)
                return urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar())), "http://127.0.0.1:" + match[1]

            def request(opener, base: str, path: str, body=None, headers=None):
                payload = None if body is None else json.dumps(body).encode()
                req = urllib.request.Request(base + path, data=payload, headers={"Content-Type": "application/json", **(headers or {})})
                with opener.open(req, timeout=30) as response:
                    raw = response.read()
                    if "application/json" in response.headers.get("Content-Type", ""):
                        return json.loads(raw)
                    return raw.decode()

            install()
            execute([*helm, "test", "metric", "--timeout", "90s"])
            if args.with_symbolicator:
                symbols = get("deployment", "metric-symbolicator")
                assert symbols["status"].get("readyReplicas") == 1
                containers = symbols["spec"]["template"]["spec"]["containers"]
                assert {c["name"] for c in containers} == {"symbolicator", "cleanup"}
                print("PASS bundled Symbolicator healthcheck and cleanup sidecar", flush=True)
            print("PASS fresh install, readiness and in-cluster Service access", flush=True)
            pod = pod_name()
            logs = execute([*scoped, "logs", pod, "--container", "metric"])
            token = re.search(r"METRIC_BOOTSTRAP_TOKEN=([a-f0-9]+)", logs)
            assert token, "Missing first-install bootstrap token"
            opener, base = http_session()
            assert "<html" in request(opener, base, "/").lower(), "Bundled web UI was not served"
            password = secrets.token_hex(20)
            owner = request(opener, base, "/api/v1/auth/bootstrap", {
                "setup_token": token[1], "email": "helm-owner@example.com", "display_name": "Helm Owner",
                "password": password, "organization_slug": "helm-test", "organization_name": "Helm Test",
            })
            organization = owner["organization_id"]

            def login(opener, base: str) -> dict:
                session = request(opener, base, "/api/v1/auth/login", {"email": "helm-owner@example.com", "password": password, "organization_id": str(organization)})
                return {"x-metric-organization-id": str(organization), "x-csrf-token": session["csrf_token"]}

            headers = login(opener, base)
            project = request(opener, base, "/api/v1/projects", {"slug": "persisted-project", "display_name": "Persisted project"}, headers)
            project_id = project["project_id"]
            # A small sentinel tests the PVC independently of feature-specific blob protocols.
            execute([*scoped, "exec", pod, "--", "sh", "-ec", "printf '%s' helm-persistent-fixture > /var/lib/metric/blobs/helm-volume-fixture"])
            pvc_uids = {p["metadata"]["name"]: p["metadata"]["uid"] for p in get("pvc")["items"]}
            secret_uid = get("secret", "metric-secrets")["metadata"]["uid"]
            old_uid = get("pod", pod)["metadata"]["uid"]

            # Release upgrade changes config, exercising Recreate and checksum rollout.
            values["config"] = {"retention": {"events_days": 31}}
            values_path.write_text(json.dumps(values), encoding="utf-8")
            execute([*helm, "upgrade", "metric", str(CHART), "-f", str(values_path), "--wait", "--timeout", "5m"], timeout=330)
            assert get("pod", pod_name())["metadata"]["uid"] != old_uid
            opener, base = http_session()
            headers = login(opener, base)
            assert request(opener, base, f"/api/v1/projects/{project_id}", headers=headers)["slug"] == "persisted-project"
            sentinel = execute([*scoped, "exec", pod_name(), "--", "cat", "/var/lib/metric/blobs/helm-volume-fixture"])
            assert sentinel == "helm-persistent-fixture"
            assert get("secret", "metric-secrets")["metadata"]["uid"] == secret_uid
            print("PASS configuration upgrade: new pod, existing account/project, blobs and secrets", flush=True)

            forward.terminate()
            forward.wait(timeout=10)
            forward = None
            execute([*helm, "uninstall", "metric", "--wait", "--timeout", "3m"])
            assert {p["metadata"]["name"]: p["metadata"]["uid"] for p in get("pvc")["items"]} == pvc_uids
            assert get("secret", "metric-secrets")["metadata"]["uid"] == secret_uid
            # Explicit existingClaim selects the retained disks after reinstall.
            values["persistence"] = {"existingClaim": "metric-blobs"}
            values["mongodb"] = {"persistence": {"existingClaim": "metric-mongodb"}}
            values_path.write_text(json.dumps(values), encoding="utf-8")
            install()
            opener, base = http_session()
            headers = login(opener, base)
            assert request(opener, base, f"/api/v1/projects/{project_id}", headers=headers)["slug"] == "persisted-project"
            assert execute([*scoped, "exec", pod_name(), "--", "cat", "/var/lib/metric/blobs/helm-volume-fixture"]) == "helm-persistent-fixture"
            assert {p["metadata"]["name"]: p["metadata"]["uid"] for p in get("pvc")["items"]} == pvc_uids
            print("PASS uninstall/reinstall: retained MongoDB, BlobStore and credentials", flush=True)
            execute([*helm, "test", "metric", "--timeout", "90s"])
            success = True
    finally:
        if forward is not None:
            forward.terminate()
            forward.wait(timeout=10)
        if created:
            if not success:
                print(execute([*scoped, "get", "pods,pvc,events"]), flush=True)
            if success or not args.keep_on_failure:
                # This script created the unique namespace and every record inside it.
                execute([*kube, "delete", "namespace", namespace, "--wait=true", "--timeout=120s"])
                print("Removed disposable test namespace and its fixture data.", flush=True)
            else:
                print(f"Kept failed test namespace {namespace} for inspection.", flush=True)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (AssertionError, RuntimeError, urllib.error.URLError, subprocess.TimeoutExpired) as error:
        print(f"Helm cluster test failed: {error}", file=sys.stderr)
        sys.exit(1)
