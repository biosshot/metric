#!/usr/bin/env python3
"""Docker/native PostgreSQL gates and shared Mongo functional checks; synthetic data only."""
from __future__ import annotations

import argparse
import concurrent.futures
import contextlib
import datetime as dt
import http.cookiejar
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
import uuid
import zlib

ROOT = Path(__file__).resolve().parents[1]
CONTAINER = "metric-pg-dev-postgres-1"
MONGO_CONTAINER = "metric-pg-baseline-mongodb-1"
ADMIN_URI = "postgres://metric_test:metric-postgres-local-only@127.0.0.1:54329/metric_test"
HMAC = "07" * 32
CRUD_CASES = ["API token create/list/scoped access/revoke", "DSN key create/list/use/disable",
    "Issue resolve/idempotent retry/reopen", "Saved query create/read/update/delete/scope/revision",
    "Dashboard create/read/update/delete/scope/revision", "Project deletion request/cancel"]


def run(args, *, env=None, timeout=600, capture=False, cwd=ROOT, combine=False):
    result = subprocess.run(args, cwd=cwd, env=env, timeout=timeout,
                            text=True, encoding="utf-8", errors="replace",
                            stdout=subprocess.PIPE if capture else None,
                            stderr=subprocess.PIPE if capture else None)
    if result.returncode:
        # Captured sender/server diagnostics can contain DSNs/tokens. Do not echo.
        raise RuntimeError(f"{Path(str(args[0])).name} failed (exit {result.returncode})")
    return result.stdout + (result.stderr if combine else "") if capture else ""


def psql(database, statement):
    return run(["docker", "exec", CONTAINER, "psql", "-U", "metric_test", "-d",
                database, "-v", "ON_ERROR_STOP=1", "-Atc", statement], capture=True).strip()


def mongo(database, statement):
    # Only UUID namespaces owned by this coordinator may be inspected/removed.
    if not re.fullmatch(r"metric_mongo_runtime_[0-9a-f]{32}", database):
        raise RuntimeError("unowned Mongo test namespace")
    script = f"const fixture = db.getSiblingDB({json.dumps(database)}); " + statement
    return run(["docker", "exec", MONGO_CONTAINER, "mongosh", "--quiet",
        "--username", "metric", "--password", "metric-local-only",
        "--authenticationDatabase", "admin", "--eval", script], capture=True, timeout=40).strip()


def require_docker(fixture=True):
    run(["docker", "info", "--format", "{{.ServerVersion}}"], capture=True, timeout=20)
    if not fixture:
        return
    state = run(["docker", "inspect", "--format", "{{.State.Health.Status}}", CONTAINER],
                capture=True, timeout=20).strip()
    if state != "healthy":
        raise RuntimeError("isolated PostgreSQL fixture must be healthy; no silent skip")
    if psql("metric_test", "SELECT 1") != "1":
        raise RuntimeError("native PostgreSQL fixture unavailable")


def adapter_suite(suite):
    env = os.environ.copy()
    env["METRIC_TEST_POSTGRES_URI"] = ADMIN_URI
    certs = ROOT / "target/postgres-test-tls"
    if suite in ("adapter", "security"):
        if not all((certs / name).is_file() for name in ("ca.crt", "wrong-ca.crt", "client.crt", "client.key")):
            raise RuntimeError("local TLS fixture certificates required; no silent skip")
        env.update({"METRIC_TEST_POSTGRES_TLS_URI": ADMIN_URI.replace(":54329/", ":54330/"),
                    "METRIC_TEST_POSTGRES_TLS_ROOT": str(certs / "ca.crt"),
                    "METRIC_TEST_POSTGRES_TLS_WRONG_ROOT": str(certs / "wrong-ca.crt"),
                    "METRIC_TEST_POSTGRES_TLS_CLIENT_CERT": str(certs / "client.crt"),
                    "METRIC_TEST_POSTGRES_TLS_CLIENT_KEY": str(certs / "client.key")})
    filters = {"adapter": [""], "security": ["connection::tests", "explore::tests"],
               "migrations": ["migrations::tests"], "faults": ["commit"]}[suite]
    count = 0
    for case_filter in filters:
        command = ["cargo", "test", "-p", "metric-postgres", "--lib", "--all-features", "--locked", "-j1"]
        if case_filter:
            command.append(case_filter)
        command.extend(["--", "--ignored", "--test-threads=1"])
        output = run(command, env=env, capture=True, timeout=900)
        matches = re.findall(r"test result: ok\. (\d+) passed; 0 failed;", output)
        if not matches or int(matches[-1]) == 0:
            raise RuntimeError("expected real-adapter cases did not run")
        count += int(matches[-1])
    return {"suite": suite, "passed": count}


class Http:
    def __init__(self, base):
        self.base = base
        self.headers = {}
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}),
            urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))

    def request(self, path, body=None, method=None, expected=200, headers=None, raw_bytes=False):
        data = json.dumps(body).encode() if body is not None else None
        request = urllib.request.Request(self.base + path, data=data,
            method=method, headers={**self.headers, "Content-Type": "application/json", **(headers or {})})
        try:
            response = self.opener.open(request, timeout=15)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            raw = response.read(16 * 1024 * 1024 + 1)
            if len(raw) > 16 * 1024 * 1024:
                raise RuntimeError("HTTP response exceeded fixture byte budget")
            if response.code != expected:
                raise RuntimeError(f"HTTP {method or ('POST' if data else 'GET')} {path.split('?')[0]}: expected {expected}, got {response.code}")
            if raw_bytes:
                return raw
            return json.loads(raw) if raw and response.headers.get("Content-Type", "").startswith("application/json") else None

    def ready(self):
        try:
            self.request("/ready")
            return True
        except (RuntimeError, OSError):
            return False


class ContainerHttp(Http):
    """Access offline app over container loopback; no host publishing dependency."""
    def __init__(self, container):
        self.container = container
        self.base = "http://127.0.0.1:4001"
        self.headers = {}
        self.cookie_path = "/tmp/metric-pg-test-cookies-" + uuid.uuid4().hex

    def request(self, path, body=None, method=None, expected=200, headers=None, raw_bytes=False):
        command = ["docker", "exec", "-i", self.container, "curl", "--silent", "--show-error",
            "--max-time", "15", "--max-filesize", str(16 * 1024 * 1024),
            "--cookie", self.cookie_path, "--cookie-jar", self.cookie_path,
            "--write-out", "\n%{http_code}", "--request", method or ("POST" if body is not None else "GET")]
        for name, value in {**self.headers, "Content-Type": "application/json", **(headers or {})}.items():
            command.extend(["--header", f"{name}: {value}"])
        if body is not None:
            command.extend(["--data-binary", "@-"])
        command.append(self.base + path)
        response = subprocess.run(command, cwd=ROOT, input=json.dumps(body).encode() if body is not None else b"",
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=20)
        if response.returncode:
            raise RuntimeError("offline container HTTP transport failed; captured diagnostics withheld")
        raw, code = response.stdout.rsplit(b"\n", 1)
        if int(code) != expected:
            raise RuntimeError(f"offline HTTP {path.split('?')[0]}: expected {expected}, got {int(code)}")
        if len(raw) > 16 * 1024 * 1024:
            raise RuntimeError("offline HTTP response exceeded fixture byte budget")
        if raw_bytes:
            return raw
        if not raw:
            return None
        try:
            return json.loads(raw)
        except json.JSONDecodeError:
            return None


class NativeServer:
    def __init__(self, binary, config, env):
        self.binary, self.config, self.env = binary, config, env
        self.process = None
        self.token = None
        self.lines = []
        self.reader = None

    def start(self):
        self.process = subprocess.Popen([str(self.binary), "--config", str(self.config)],
            cwd=ROOT, env=self.env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            text=True, encoding="utf-8", errors="replace")
        def read():
            for line in self.process.stdout:
                match = re.search(r"METRIC_BOOTSTRAP_TOKEN=([0-9a-f]+)", line)
                if match:
                    self.token = match[1]
                    line = "[bootstrap token redacted]\n"
                if len(self.lines) < 2000:
                    self.lines.append(line)
        self.reader = threading.Thread(target=read, daemon=True)
        self.reader.start()

    def wait_ready(self, http):
        until = time.monotonic() + 90
        while time.monotonic() < until:
            if self.process.poll() is not None:
                raise RuntimeError("native server exited during startup; captured diagnostics withheld")
            if http.ready():
                return
            time.sleep(0.1)
        raise RuntimeError("native server readiness deadline exceeded")

    def stop(self):
        if self.process is not None:
            if self.process.poll() is None:
                # Windows lacks POSIX SIGTERM. Graceful signal is separately tested
                # in the Docker container suite; this is a real process-restart fault.
                self.process.terminate()
                self.process.wait(timeout=30)
            self.reader.join(timeout=3)
            self.process.stdout.close()
            self.process = None


def sdk_events(http, project, use_browser=False):
    dsn = f"http://{project['dsn_key']}@{http.base.removeprefix('http://')}/{project['project_id']}"
    classpath = os.pathsep.join([str(ROOT / "sdk-tests/java/.deps/sentry-8.50.1.jar"),
                                str(ROOT / "sdk-tests/java/.deps/classes")])
    senders = {
        "javascript_node": (["node", "sdk-tests/node/send-error.mjs", dsn], ROOT),
        "python": ([sys.executable, "sdk-tests/python/send_error.py", dsn], ROOT),
        "java": (["java", "-cp", classpath, "MetricSdkCompatibility", dsn], ROOT),
        "dotnet": (["dotnet", "run", "--project", "sdk-tests/dotnet/MetricSdkCompatibility.csproj",
                    "--configuration", "Release", "--no-build", "--no-restore", "--", dsn], ROOT),
        "go": (["go", "run", ".", dsn], ROOT / "sdk-tests/go"),
        "rust": (["cargo", "run", "--quiet", "--locked", "-j1", "--manifest-path", "sdk-tests/rust/Cargo.toml", "--", dsn], ROOT),
        "ruby": (["ruby", "sdk-tests/ruby/send_error.rb", dsn], ROOT),
        "php": ([sys.executable, "sdk-tests/php/run.py", dsn], ROOT),
    }
    evidence = []
    for name, (command, cwd) in senders.items():
        output = run(command, cwd=cwd, timeout=180, capture=True)
        result = json.loads(output.strip().splitlines()[-1])
        if not result.get("flushed"):
            raise RuntimeError(f"{name} SDK did not flush")
        wait_event(http, project["project_id"], result["event_id"], label=name)
        evidence.append(name)
    # Reuse official SDK fixtures against the complete selected runtime.
    def node_fixture(filename):
        output = run(["node", f"sdk-tests/node/{filename}", dsn], timeout=45, capture=True)
        return json.loads(output.strip().splitlines()[-1])
    attached = node_fixture("send-attachment.mjs")
    attachment_event = wait_event(http, project["project_id"], attached["event_id"], label="javascript_node_attachment")
    root = f"/api/v1/projects/{project['project_id']}"
    attachments = http.request(f"{root}/events/{attachment_event['event_id']}/attachments")["items"]
    if len(attachments) != 1 or attachments[0]["filename"] != "metric-context.json":
        raise RuntimeError("real Node attachment metadata did not persist")
    data = http.request(f"{root}/events/{attachment_event['event_id']}/attachments/{attachments[0]['attachment_id']}", raw_bytes=True)
    if json.loads(data) != {"source": "node-sdk", "safe": True}:
        raise RuntimeError("real Node attachment Blob bytes changed")
    evidence.append("javascript_node_attachment")
    # Session ingestion uses a derived release identity; the management API
    # separately requires an explicit release before exposing its health.
    release = http.request(f"{root}/releases", {"version": "metric-node-sessions@1.0.0", "repositories": []})
    sessions = node_fixture("send-sessions.mjs")
    if not sessions.get("flushed"):
        raise RuntimeError("real Node Sessions did not flush")
    until = time.monotonic() + 30
    while True:
        health = http.request(f"{root}/releases/{release['id']}/health")["items"]
        # The installed Node SDK first closes its implicit session, then sends
        # the explicit healthy lifecycle and crashed session: three unique SIDs.
        if sum(int(x["sessions"]) for x in health) == 3 and sum(int(x["crashed"]) for x in health) == 1 and sum(int(x["exited"]) for x in health) == 2:
            break
        if time.monotonic() >= until:
            raise RuntimeError(f"real Node Sessions health did not persist: buckets={len(health)}, sessions={sum(int(x['sessions']) for x in health)}, crashed={sum(int(x['crashed']) for x in health)}")
        time.sleep(.1)
    evidence.append("javascript_node_sessions")
    node_fixture("send-cron.mjs")
    monitors = http.request(f"{root}/monitors")["items"]
    expected = {"metric-node-cron": "success", "metric-node-cron-error": "error"}
    if {m["slug"]: m["last_status"] for m in monitors} != expected:
        raise RuntimeError("real Node cron terminal states did not persist")
    for monitor in monitors:
        runs = http.request(f"{root}/monitors/{monitor['id']}/runs")["items"]
        if len(runs) != 1 or runs[0]["status"] != expected[monitor["slug"]]:
            raise RuntimeError("real Node cron run history changed")
    evidence.append("javascript_node_cron")
    metrics = node_fixture("send-metrics.mjs")
    if not metrics.get("flushed"):
        raise RuntimeError("real Node Metrics did not flush")
    query = {"source": "metrics", "query": "", "result": {"kind": "records"}, "limit": 10}
    rows = http.request(f"{root}/query", query)["items"]
    if len(rows) != 3:
        raise RuntimeError("real Node metric buckets did not persist")
    evidence.append("javascript_node_metrics")
    run(["node", "sdk-tests/node/send-signals.mjs", dsn], timeout=45, capture=True)
    for source in ("logs", "traces"):
        result = http.request(f"{root}/query", {"source": source, "query": "", "result": {"kind": "records"}, "limit": 10})
        if not result["items"]:
            raise RuntimeError(f"real Node {source} did not persist")
    evidence.append("javascript_node_signals")
    if use_browser:
        page = ROOT / "sdk-tests/browser/page.html"
        bundle = ROOT / "sdk-tests/browser/dist/send-error.js"
        if not page.is_file() or not bundle.is_file():
            raise RuntimeError("installed/built browser SDK fixture required; no silent skip")
        playback = ROOT / "sdk-tests/browser/dist/play-replay.js"
        playback_css = ROOT / "sdk-tests/browser/dist/play-replay.css"
        recording = b""
        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                path = self.path.split("?", 1)[0]
                asset = {"/": page, "/sdk-browser.js": bundle, "/sdk-playback.js": playback, "/play-replay.css": playback_css}.get(path)
                content = recording if path == "/sdk-replay-recording" else None
                if path == "/sdk-playback":
                    content = page.read_bytes().replace(b"/sdk-browser.js", b"/sdk-playback.js")
                self.send_response(200 if asset or content is not None else 404)
                self.send_header("Content-Type", "text/javascript" if path.endswith(".js") else "text/css" if path.endswith(".css") else "text/html" if content is None or path == "/sdk-playback" else "application/octet-stream")
                self.end_headers()
                if content is not None:
                    self.wfile.write(content)
                elif asset:
                    self.wfile.write(asset.read_bytes())
            def log_message(self, *_args):
                pass
        listener = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        worker = threading.Thread(target=listener.serve_forever, daemon=True)
        worker.start()
        try:
            output = run(["node", "sdk-tests/browser/run-browser.mjs",
                f"http://127.0.0.1:{listener.server_port}/", dsn], timeout=45, capture=True)
            result = json.loads(output.strip().splitlines()[-1])
            if not result.get("flushed"):
                raise RuntimeError("real browser SDK did not flush")
            wait_event(http, project["project_id"], result["event_id"], label="javascript_browser")
            evidence.append("javascript_browser")
            bundle = ROOT / "sdk-tests/browser/dist/send-feedback.js"
            output = run(["node", "sdk-tests/browser/run-browser.mjs", f"http://127.0.0.1:{listener.server_port}/", dsn], timeout=45, capture=True)
            result = json.loads(output.strip().splitlines()[-1])
            feedback_id = uuid.UUID(result["event_id"]).hex
            feedback = http.request(f"{root}/feedback/{feedback_id}")
            if feedback["message"] != "The checkout button did not respond" or len(feedback["attachments"]) != 1:
                raise RuntimeError("real Browser Feedback fields did not persist")
            attachment_id = feedback["attachments"][0]["attachment_id"]
            if http.request(f"{root}/feedback/{feedback_id}/attachments/{attachment_id}", raw_bytes=True) != b"safe browser feedback context":
                raise RuntimeError("real Browser Feedback Blob bytes changed")
            evidence.append("javascript_browser_feedback")
            bundle = ROOT / "sdk-tests/browser/dist/send-replay.js"
            output = run(["node", "sdk-tests/browser/run-browser.mjs", f"http://127.0.0.1:{listener.server_port}/", dsn], timeout=45, capture=True)
            result = json.loads(output.strip().splitlines()[-1])
            replay_id = uuid.UUID(result["replay_id"]).hex
            replay = http.request(f"{root}/replays/{replay_id}")
            if not replay["segments"] or replay["release"] != "metric-browser-replay-test@1.0.0":
                raise RuntimeError("real Browser Replay metadata did not persist")
            segment = replay["segments"][0]
            recording = http.request(f"{root}/replays/{replay_id}/segments/{segment['segment_id']}", raw_bytes=True)
            header, payload = recording.split(b"\n", 1)
            decoded = payload if payload.startswith(b"[") else zlib.decompress(payload)
            if b"top-secret-value" in decoded or len(json.loads(decoded)) < 2:
                raise RuntimeError("real Browser Replay privacy/recording integrity failed")
            output = run(["node", "sdk-tests/browser/run-browser.mjs", f"http://127.0.0.1:{listener.server_port}/sdk-playback", dsn], timeout=45, capture=True)
            played = json.loads(output.strip().splitlines()[-1])
            if played["event_id"] != "0123456789abcdef0123456789abcdef":
                raise RuntimeError("real rrweb player did not complete retrieved PostgreSQL Replay")
            evidence.append("javascript_browser_replay")
        finally:
            listener.shutdown()
            listener.server_close()
            worker.join(timeout=3)
    return evidence


def wait_event(http, project_id, event_id, label="native Event"):
    # SDKs may print a UUID with hyphens; the native API uses canonical hex32.
    event_id = uuid.UUID(event_id).hex
    until = time.monotonic() + 30
    while time.monotonic() < until:
        try:
            return http.request(f"/api/v1/projects/{project_id}/events/{event_id}")
        except RuntimeError as error:
            if "got 404" not in str(error):
                raise
        time.sleep(0.1)
    raise RuntimeError(f"{label} did not finalize before deadline")


@contextlib.contextmanager
def migration_owner(database):
    owner = subprocess.Popen(["docker", "exec", CONTAINER, "psql", "-U", "metric_test", "-d", database,
        "-v", "ON_ERROR_STOP=1", "-Atc",
        f"SET application_name='metric-test-migration-owner'; SELECT pg_advisory_lock({0x4d45545249435047}); SELECT pg_sleep(120)"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        until = time.monotonic() + 10
        while time.monotonic() < until:
            held = psql(database, "SELECT EXISTS(SELECT 1 FROM pg_locks l JOIN pg_stat_activity a ON a.pid=l.pid "
                "WHERE l.locktype='advisory' AND l.granted AND a.application_name='metric-test-migration-owner')")
            if held == "t":
                break
            if owner.poll() is not None:
                raise RuntimeError("synthetic migration lock owner exited early")
            time.sleep(0.1)
        else:
            raise RuntimeError("synthetic migration lock ownership deadline exceeded")
        yield
    finally:
        # Unique database created by this invocation; terminate only our named owner.
        psql(database, "SELECT pg_terminate_backend(pid) FROM pg_stat_activity "
              "WHERE application_name='metric-test-migration-owner' AND pid<>pg_backend_pid()")
        owner.wait(timeout=10)


def exercise_http(http, setup_token, use_sdk=False, use_browser=False, backend="postgres"):
    context = http.request("/api/v1/auth/bootstrap", {"setup_token": setup_token,
        "email": "postgres-test@example.invalid", "display_name": "Synthetic Owner",
        "password": "synthetic correct horse battery staple", "organization_slug": "pg-test",
        "organization_name": "Synthetic PostgreSQL"})
    login = http.request("/api/v1/auth/login", {"email": "postgres-test@example.invalid",
        "password": "synthetic correct horse battery staple", "organization_id": context["organization_id"]})
    http.headers.update({"x-metric-organization-id": context["organization_id"], "x-csrf-token": login["csrf_token"]})
    status = http.request("/api/v1/status")
    inactive = "mongodb" if backend == "postgres" else "postgres"
    if status["components"].get(backend) != "available" or inactive in status["components"]:
        raise RuntimeError("component status did not identify the selected storage backend")
    if use_browser:
        run(["node", "web/tests/e2e/real-session.mjs", http.base,
             "postgres-test@example.invalid", "synthetic correct horse battery staple",
             context["organization_id"]], timeout=90, capture=True)
        # The existing login policy revokes prior sessions for this user. Obtain
        # a new coordinator session after the independent browser login.
        login = http.request("/api/v1/auth/login", {"email": "postgres-test@example.invalid",
            "password": "synthetic correct horse battery staple", "organization_id": context["organization_id"]})
        http.headers["x-csrf-token"] = login["csrf_token"]
        listed = http.request("/api/v1/projects")["items"]
        if len(listed) != 1 or listed[0]["slug"] != "backend" or listed[0]["policy"]["ip_policy"] != "remove":
            raise RuntimeError("real browser UI project/policy mutation did not persist")
    project = http.request("/api/v1/projects", {"slug": "pg-native", "display_name": "Native fixture", "replay_enabled": True}, expected=201)
    other = http.request("/api/v1/projects", {"slug": "pg-other", "display_name": "Other scope"}, expected=201)
    event_id = uuid.uuid4().hex
    body = {"event_id": event_id, "platform": "python", "level": "error",
            "timestamp": dt.datetime.now(dt.timezone.utc).isoformat(),
            "exception": {"values": [{"type": "NativePostgresFixtureError", "value": "quotes ' ; -- Unicode Ж"}]},
            "request": {"headers": {"Authorization": "NEVER_PERSIST_AUTHORIZATION"}}}
    ingest = f"/api/{project['project_id']}/store/?sentry_key={project['dsn_key']}"
    http.request(ingest, body)
    event = wait_event(http, project["project_id"], event_id)
    if "NEVER_PERSIST_AUTHORIZATION" in json.dumps(event):
        raise RuntimeError("Event scrubbing did not precede persistence")
    http.request(ingest, body)
    time.sleep(0.2)
    issue = http.request(f"/api/v1/projects/{project['project_id']}/issues/{event['issue_id']}")
    if int(issue["occurrence_count"]) != 1:
        raise RuntimeError("duplicate Event double-applied finalization")
    http.request(f"/api/v1/projects/{other['project_id']}/events/{event_id}", expected=404)
    unauthorized = Http(http.base) if not isinstance(http, ContainerHttp) else ContainerHttp(http.container)
    unauthorized.request(f"/api/v1/projects/{project['project_id']}/events/{event_id}", expected=401)
    query = {"source": "errors", "query": "", "result": {"kind": "records"}, "limit": 10}
    http.request(f"/api/v1/projects/{project['project_id']}/query", query)
    sdk = sdk_events(http, project, use_browser) if use_sdk else []
    exercise_crud(http, project, event, other)
    return project, event, sdk


def server_binary(args):
    binary = Path(args.binary or ROOT / ("target/debug/metric-server.exe" if os.name == "nt" else "target/debug/metric-server"))
    if not args.binary:
        run(["cargo", "build", "--bin", "metric-server", "--all-features", "--locked", "-j1"])
    if not binary.is_file():
        raise RuntimeError("Metric server binary required")
    return binary


def exercise_migration_wait(http, database):
    until = time.monotonic() + 10
    while True:
        try:
            http.request("/live")
            break
        except OSError:
            if time.monotonic() >= until:
                raise RuntimeError("maintenance listener did not start")
            time.sleep(0.1)
    until = time.monotonic() + 10
    while True:
        progress = http.request("/api/v1/projects", expected=503)
        if progress["status"] == "waiting_for_migration":
            break
        if time.monotonic() >= until:
            raise RuntimeError("migration ownership wait was not reported")
        time.sleep(0.1)
    http.request("/ready", expected=503)
    if psql(database, "SELECT to_regclass('metric._sqlx_migrations') IS NOT NULL") != "f":
        raise RuntimeError("waiting application changed the schema without ownership")


def exercise_crud(http, project, event, other):
    prefix = f"/api/v1/projects/{project['project_id']}"
    foreign = f"/api/v1/projects/{other['project_id']}"
    expires = (dt.datetime.now(dt.timezone.utc) + dt.timedelta(hours=1)).isoformat()
    token = http.request("/api/v1/auth/tokens", {"name": "functional-token",
        "scopes": ["event:read", "issue:read"], "expires_at": expires}, expected=201)
    listed = http.request("/api/v1/auth/tokens")["items"]
    if not any(row["id"] == token["id"] for row in listed) or token["token"] in json.dumps(listed):
        raise RuntimeError("API token list lost metadata or exposed its secret")
    client = Http(http.base) if not isinstance(http, ContainerHttp) else ContainerHttp(http.container)
    client.headers["Authorization"] = "Bearer " + token["token"]
    client.request(prefix + "/events/" + event["event_id"])
    client.request(prefix + "/keys", {"label": "must-be-forbidden"}, expected=403)
    http.request("/api/v1/auth/tokens/" + token["id"], method="DELETE", expected=204)
    client.request(prefix + "/events/" + event["event_id"], expected=401)

    key = http.request(prefix + "/keys", {"label": "functional-key"}, expected=201)["dsn_key"]
    if not any(row["dsn_key"] == key for row in http.request(prefix + "/keys")["items"]):
        raise RuntimeError("new DSN key was not listed")
    payload = {"event_id": event["event_id"], "platform": "python", "message": "duplicate fixture"}
    path = f"/api/{project['project_id']}/store/?sentry_key={key}"
    http.request(path, payload)
    http.request(prefix + "/keys/" + key, method="DELETE", expected=204)
    http.request(path, payload, expected=401)
    http.request(prefix + "/events/" + event["event_id"])

    issue_path = prefix + "/issues/" + event["issue_id"]
    command = {"action": "resolve", "idempotency_key": uuid.uuid4().hex}
    first = http.request(issue_path + "/lifecycle", command)
    again = http.request(issue_path + "/lifecycle", command)
    if not first["applied"] or again["applied"] or again["issue"]["status"] != "resolved":
        raise RuntimeError("Issue lifecycle retry was not idempotent")
    http.request(issue_path + "/lifecycle", {"action": "reopen", "idempotency_key": uuid.uuid4().hex})
    if http.request(issue_path)["status"] != "open":
        raise RuntimeError("Issue reopen did not persist")

    now = int(time.time() * 1000)
    query = {"dataset": "errors", "from": now - 3_600_000, "until": now + 60_000,
        "limit": 10, "query": "level:error"}
    saved = http.request(prefix + "/saved-queries", {"name": "functional query ' Ж", "query": query}, expected=201)
    saved_path = prefix + "/saved-queries/" + saved["id"]
    if http.request(saved_path)["query"] != saved["query"]:
        raise RuntimeError("saved query changed after persistence")
    http.request(foreign + "/saved-queries/" + saved["id"], expected=404)
    updated = {"revision": saved["revision"], "name": "updated query", "query": query}
    saved = http.request(saved_path, updated, method="PATCH")
    http.request(saved_path, updated, method="PATCH", expected=409)
    dashboard_input = {"name": "functional dashboard", "widgets": [{"title": "Errors",
        "saved_query_id": saved["id"], "shape": "table"}], "refresh_interval": "manual"}
    dashboard = http.request(prefix + "/dashboards", dashboard_input, expected=201)
    dashboard_path = prefix + "/dashboards/" + dashboard["id"]
    if http.request(dashboard_path)["widgets"] != dashboard["widgets"]:
        raise RuntimeError("dashboard widgets changed after persistence")
    http.request(foreign + "/dashboards/" + dashboard["id"], expected=404)
    dashboard_input.update(revision=dashboard["revision"], name="updated dashboard", widgets=dashboard["widgets"])
    http.request(dashboard_path, dashboard_input, method="PATCH")
    http.request(dashboard_path, dashboard_input, method="PATCH", expected=409)
    http.request(dashboard_path, method="DELETE", expected=204)
    http.request(dashboard_path, expected=404)
    http.request(saved_path, method="DELETE", expected=204)
    http.request(saved_path, expected=404)

    operation = uuid.uuid4().hex
    http.request(foreign, {"confirm_slug": http.request(foreign)["slug"]}, method="DELETE", expected=202,
        headers={"idempotency-key": operation})
    http.request(foreign + "/deletion/cancel", {"operation_id": operation})
    http.request(foreign)


def lifecycle_counts(database, backend, project_id):
    project_id = int(project_id)
    # Static fixture-owned identifiers, generated numeric project identity only.
    names = ("error_events", "issues", "logs", "spans", "sessions", "feedback",
        "monitors", "monitor_runs", "metric_buckets", "replays", "project_keys")
    if backend == "postgres":
        pairs = ",".join(f"'{name}',(SELECT count(*) FROM metric.{name} WHERE project_id={project_id})" for name in names)
        return json.loads(psql(database, f"SELECT json_build_object({pairs})"))
    script = "const counts={}; "
    for name in names:
        field = "project_id" if name == "project_keys" else "p"
        script += f"counts.{name}=fixture.{name}.countDocuments({{{field}:{project_id}}}); "
    return json.loads(mongo(database, script + "print(JSON.stringify(counts))"))


def exercise_purge(http, project, database, backend, all_sdk):
    prefix = f"/api/v1/projects/{project['project_id']}"
    before = lifecycle_counts(database, backend, project["project_id"])
    required = ("error_events", "issues", "project_keys")
    if all_sdk:
        required = tuple(before)
    if any(before[name] == 0 for name in required):
        raise RuntimeError("purge fixture did not populate each required record family")
    slug = http.request(prefix)["slug"]
    http.request(prefix, {"confirm_slug": slug}, method="DELETE", expected=202,
        headers={"idempotency-key": uuid.uuid4().hex})
    until = time.monotonic() + 60
    while True:
        status = http.request(prefix + "/deletion")
        if status["phase"] == "deleted":
            break
        if time.monotonic() >= until:
            raise RuntimeError("project purge did not complete before its functional deadline")
        time.sleep(0.2)
    after = lifecycle_counts(database, backend, project["project_id"])
    if any(after.values()):
        raise RuntimeError("project purge left owned records: " + json.dumps(after))
    # A tombstone may remain visible to administrators; no authoritative payload survives.
    event_id = uuid.uuid4().hex
    http.request(f"/api/{project['project_id']}/store/?sentry_key={project['dsn_key']}",
        {"event_id": event_id, "message": "must not survive deletion"}, expected=401)
    return {"populated": before, "remaining": after}


def restore_owned_fixture(database, backend, directory):
    """Round-trip only a UUID database created by this coordinator, while stopped."""
    prefix = 'metric_pg_runtime_' if backend == 'postgres' else 'metric_mongo_runtime_'
    if not re.fullmatch(prefix + r'[0-9a-f]{32}', database):
        raise RuntimeError('Refusing backup/restore of a foreign database')
    archive = directory / 'fixture-backup.archive'
    if backend == 'postgres':
        dump = ['docker', 'exec', CONTAINER, 'pg_dump', '-U', 'metric_test', '-d', database, '-Fc']
        restore = ['docker', 'exec', '-i', CONTAINER, 'pg_restore', '-U', 'metric_test', '-d', database, '--exit-on-error']
    else:
        auth = ['--username', 'metric', '--password', 'metric-local-only', '--authenticationDatabase', 'admin']
        dump = ['docker', 'exec', MONGO_CONTAINER, 'mongodump', *auth, '--db', database, '--archive', '--gzip']
        restore = ['docker', 'exec', '-i', MONGO_CONTAINER, 'mongorestore', *auth, '--archive', '--gzip', '--stopOnError']
    with archive.open('wb') as output:
        subprocess.run(dump, stdout=output, stderr=subprocess.PIPE, check=True, timeout=120)
    if not archive.stat().st_size:
        raise RuntimeError('Empty synthetic backup')
    if backend == 'postgres':
        psql('metric_test', f'DROP DATABASE {database}')
        psql('metric_test', f'CREATE DATABASE {database}')
    else:
        mongo(database, 'fixture.dropDatabase()')
    with archive.open('rb') as source:
        subprocess.run(restore, stdin=source, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True, timeout=120)


def server_suite(args):
    binary = server_binary(args)
    backend = args.backend
    database = ("metric_pg_runtime_" if backend == "postgres" else "metric_mongo_runtime_") + uuid.uuid4().hex
    # Only an identifier generated in this invocation is interpolated into DDL.
    if backend == "postgres":
        assert re.fullmatch(r"metric_pg_runtime_[0-9a-f]{32}", database)
        psql("metric_test", f"CREATE DATABASE {database}")
    else:
        if mongo(database, "print(fixture.getCollectionNames().length)") != "0":
            raise RuntimeError("synthetic Mongo namespace already exists")
    temp_root = ROOT / "target/postgres-runtime-tests"
    temp_root.mkdir(parents=True, exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix="runtime-", dir=temp_root))
    env = os.environ.copy()
    for name in list(env):
        if name.startswith("APP__") or name in ("PGOPTIONS", "PGSSLROOTCERT", "PGSSLCERT", "PGSSLKEY"):
            del env[name]
    env["METRIC_POSTGRES_TEST_URI"] = ADMIN_URI.rsplit("/", 1)[0] + "/" + database
    env["METRIC_POSTGRES_TEST_HMAC"] = HMAC
    env["METRIC_MONGO_TEST_URI"] = "mongodb://metric:metric-local-only@127.0.0.1:27018/?authSource=admin&serverSelectionTimeoutMS=2000&connectTimeoutMS=2000"
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    config = directory / "metric.toml"
    source = (ROOT / "deploy/metric.postgres.test.toml").read_text(encoding="utf-8")
    source = source.replace('backend = "postgres"', f'backend = "{backend}"', 1)
    config.write_text(source
        .replace('http_address = "0.0.0.0:4001"', f'http_address = "127.0.0.1:{port}"')
        .replace('root = "/var/lib/metric/blobs"', f'root = "{directory.as_posix()}/blobs"'), encoding="utf-8")
    server = NativeServer(binary, config, env)
    try:
        # Explicit selection must not resolve unused Mongo secrets; CLI performs
        # no network connection and must redact both selected URI/HMAC values.
        with config.open("a", encoding="utf-8") as f:
            if backend == "postgres":
                f.write('\n[mongodb]\nuri={env="METRIC_TEST_UNUSED_MONGO_MUST_NOT_RESOLVE"}\n')
            else:
                f.write(f'\n[mongodb]\nuri={{env="METRIC_MONGO_TEST_URI"}}\ndatabase="{database}"\n')
            if args.suite == "load":
                f.write('\n[scheduler]\nmaintenance_interval="2s"\n')
            if args.verify_purge:
                f.write('\n[project_deletion]\ngrace_period="3s"\npoll_interval="100ms"\ndelete_batch_documents=32\n')
        run([str(binary), "--config", str(config), "--check-config"], env=env, capture=True)
        effective = run([str(binary), "--config", str(config), "--print-effective-config"], env=env, capture=True)
        if any(secret in effective for secret in (HMAC, "metric-postgres-local-only", "metric-local-only")):
            raise RuntimeError("effective configuration exposed fixture secrets")
        http = Http(f"http://127.0.0.1:{port}")
        with migration_owner(database) if backend == "postgres" else contextlib.nullcontext():
            server.start()
            if backend == "postgres":
                exercise_migration_wait(http, database)
        server.wait_ready(http)
        if not server.token:
            raise RuntimeError("empty synthetic installation did not emit bootstrap token")
        project, event, sdk = exercise_http(http, server.token, args.sdk, args.browser, backend)
        # Real process replacement, same native DB and Blob directory.
        server.stop()
        if args.verify_backup:
            restore_owned_fixture(database, backend, directory)
        server.start()
        server.wait_ready(http)
        recovered = wait_event(http, project["project_id"], event["event_id"])
        if recovered != event:
            raise RuntimeError("restart changed authoritative Event")
        if args.suite == "load":
            return load_suite(args, http, project, event, database, server)
        purge = exercise_purge(http, project, database, backend, args.sdk and args.browser) if args.verify_purge else None
        generation = psql(database, "SELECT generation FROM metric.schema_state") if backend == "postgres" else mongo(database, 'print(fixture.schema_meta.findOne({_id:"metric.schema"}).generation)')
        return {"suite": "server", "backend": backend, "passed": 10, "sdk": sdk, "schema_generation": int(generation), "crud": CRUD_CASES, "purge": purge, "backup_restore": args.verify_backup}
    except RuntimeError as error:
        if backend == "postgres" and "Sessions health" in str(error):
            counts = psql(database, "SELECT json_build_object('session_rows',(SELECT count(*) FROM metric.sessions),'session_states',(SELECT json_agg(x) FROM (SELECT state,count(*) FROM metric.sessions GROUP BY state ORDER BY state) x),'health_sessions',(SELECT sum(sessions) FROM metric.session_stats_hourly),'health_crashed',(SELECT sum(crashed) FROM metric.session_stats_hourly))")
            raise RuntimeError(f"{error}; synthetic DB counters: {counts}") from error
        raise
    finally:
        server.stop()
        # The exact generated DB and directory are owned by this invocation.
        if backend == "postgres":
            psql("metric_test", f"DROP DATABASE {database} WITH (FORCE)")
        else:
            mongo(database, "fixture.dropDatabase()")
        resolved = directory.resolve()
        if resolved.parent == temp_root.resolve() and resolved.name.startswith("runtime-"):
            shutil.rmtree(resolved)


def startup_suite(args):
    """Real server permanent TLS exits, distinct from temporary network failures."""
    binary = server_binary(args)
    certs = ROOT / "target/postgres-test-tls"
    for name in ("ca.crt", "wrong-ca.crt", "client.crt"):
        if not (certs / name).is_file():
            raise RuntimeError("local TLS certificates required for startup gate")
    tls_uri = ADMIN_URI.replace(":54329/", ":54330/")
    temp_root = ROOT / "target/postgres-startup-tests"
    temp_root.mkdir(parents=True, exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix="startup-", dir=temp_root))
    server = None
    try:
        malformed = directory / "private-malformed-ca.pem"
        malformed.write_text("-----BEGIN CERTIFICATE-----\nnot-a-certificate\n-----END CERTIFICATE-----\n", encoding="utf-8")
        key = directory / "private-malformed-client-key.pem"
        key.write_text("not-a-private-key", encoding="utf-8")
        cases = [
            ("wrong_ca", tls_uri, certs / "wrong-ca.crt", "TLS negotiation or certificate verification failed", {}),
            ("wrong_hostname", tls_uri.replace("127.0.0.1", "localhost"), certs / "ca.crt", "TLS negotiation or certificate verification failed", {}),
            ("tls_unsupported", ADMIN_URI, certs / "ca.crt", "TLS negotiation or certificate verification failed", {}),
            ("malformed_ca", tls_uri, malformed, "TLS negotiation or certificate verification failed", {}),
            ("malformed_client_key", tls_uri, certs / "ca.crt", "invalid PostgreSQL connection, pool or timeout settings",
                {"PGSSLCERT": str(certs / "client.crt"), "PGSSLKEY": str(key)}),
        ]
        evidence = []
        for name, uri, ca, category, extra_env in cases:
            with socket.socket() as sock:
                sock.bind(("127.0.0.1", 0))
                port = sock.getsockname()[1]
            source = (ROOT / "deploy/metric.postgres.test.toml").read_text(encoding="utf-8")
            source = source.replace("0.0.0.0:4001", f"127.0.0.1:{port}")
            source = source.replace("/var/lib/metric/blobs", (directory / "blobs").as_posix())
            source = source.replace('ssl_mode = "disable"', 'ssl_mode = "verify-full"')
            source = source.replace("allow_insecure_connection = true", "allow_insecure_connection = false")
            source = source.replace("max_connections = 10", f'max_connections = 10\nssl_root_cert = "{ca.as_posix()}"')
            config = directory / "metric.toml"
            config.write_text(source, encoding="utf-8")
            env = {k: v for k, v in os.environ.items() if not k.startswith("APP__")
                and k not in ("PGOPTIONS", "PGSSLROOTCERT", "PGSSLCERT", "PGSSLKEY")}
            env.update({"METRIC_POSTGRES_TEST_URI": uri, "METRIC_POSTGRES_TEST_HMAC": HMAC, **extra_env})
            server = NativeServer(binary, config, env)
            server.start()
            try:
                status = server.process.wait(timeout=15)
                server.reader.join(timeout=3)
                lines = "".join(server.lines)
                if status == 0 or category not in lines or "PostgreSQL startup will retry" in lines:
                    raise RuntimeError(f"{name}: protected startup did not exit with its permanent category")
                for value in (uri, "metric-postgres-local-only", HMAC, ca.as_posix(), str(ca), str(key)):
                    if value in lines:
                        raise RuntimeError(f"{name}: confidential connection data entered diagnostics")
                evidence.append(name)
            except subprocess.TimeoutExpired as error:
                raise RuntimeError(f"{name}: protected startup retried beyond its exit budget") from error
            finally:
                server.stop()
        return {"suite": "startup", "passed": len(evidence), "cases": evidence}
    finally:
        if server:
            server.stop()
        resolved = directory.resolve()
        if resolved.parent == temp_root.resolve() and resolved.name.startswith("startup-"):
            shutil.rmtree(resolved)


def resource_snapshot(database, server):
    stats = json.loads(psql(database, "SELECT json_build_object('database_bytes',pg_database_size(current_database()),"
        "'index_bytes',(SELECT coalesce(sum(pg_indexes_size(c.oid)),0) FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='metric' AND c.relkind='r'),"
        "'connections',(SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND application_name='metric'),"
        "'dead_rows',(SELECT coalesce(sum(n_dead_tup),0) FROM pg_stat_user_tables WHERE schemaname='metric'))"))
    stats["postgres_container"] = json.loads(run(["docker", "stats", "--no-stream", "--format", "{{json .}}", CONTAINER], capture=True))
    if os.name == "nt":
        # PID is generated by Popen, not user/shell input. Read only our process.
        rss = run(["powershell", "-NoProfile", "-Command", f"(Get-Process -Id {server.process.pid}).WorkingSet64"], capture=True)
        stats["server_rss_bytes"] = int(rss.strip())
    else:
        status = Path(f"/proc/{server.process.pid}/status").read_text(encoding="utf-8")
        stats["server_rss_bytes"] = int(re.search(r"VmRSS:\s+(\d+)", status)[1]) * 1024
    if stats["connections"] > 10:
        raise RuntimeError("PostgreSQL pool exceeded its configured connection bound")
    return stats


def load_suite(args, http, project, event, database, server):
    match = re.fullmatch(r"(\d+)(s|m)", args.soak_duration)
    if not match:
        raise RuntimeError("soak duration must be positive integer seconds/minutes")
    duration = int(match[1]) * (60 if match[2] == "m" else 1)
    if not 5 <= duration <= 3600:
        raise RuntimeError("soak duration must be between 5 seconds and 60 minutes")
    start = time.monotonic()
    samples = [resource_snapshot(database, server)]
    baseline_rows = int(psql(database, "SELECT count(*) FROM metric.error_events"))
    # Controlled finalized synthetic expiry proves Scheduler cleanup under ingest.
    project_id = int(project["project_id"])
    event_id = uuid.UUID(event["event_id"]).hex
    received_at = int(time.time() * 1000) - 31 * 24 * 60 * 60 * 1000
    expiry = received_at + 30 * 24 * 60 * 60 * 1000
    # Match the configured retention policy. An arbitrary expire_at=0 would be
    # correctly repaired by Scheduler before expiry, invalidating the fixture.
    psql(database, f"UPDATE metric.error_events SET received_at={received_at},expire_at={expiry} WHERE project_id={project_id} AND event_id=decode('{event_id}','hex') AND processing_state=2")
    latencies, query_latencies = [], []
    unique, duplicates, queries, ticks = 0, 0, 0, 0
    final_event = None
    next_sample = 60
    ingest = f"/api/{project_id}/store/?sentry_key={project['dsn_key']}"
    query = {"source": "errors", "query": "level:error", "result": {"kind": "records"}, "limit": 10}
    def write(body, repeat):
        client = Http(http.base)
        before = time.monotonic()
        client.request(ingest, body)
        if repeat:
            client.request(ingest, body)
        return time.monotonic() - before
    def read():
        before = time.monotonic()
        http.request(f"/api/v1/projects/{project_id}/query", query)
        return time.monotonic() - before
    with concurrent.futures.ThreadPoolExecutor(max_workers=8) as executor:
        while time.monotonic() - start < duration:
            tick_start = time.monotonic()
            count = 80 if ticks % 60 == 0 else 8
            writes = []
            for index in range(count):
                final_event = uuid.uuid4().hex
                body = {"event_id": final_event, "platform": "python", "level": "error",
                    "timestamp": dt.datetime.now(dt.timezone.utc).isoformat(),
                    "exception": {"values": [{"type": "PostgresSoakError", "value": "bounded synthetic soak"}]}}
                repeat = index % 8 == 0
                writes.append(executor.submit(write, body, repeat))
                unique += 1
                duplicates += int(repeat)
            read_future = executor.submit(read)
            latencies.extend(f.result() for f in writes)
            query_latencies.append(read_future.result())
            queries += 1
            ticks += 1
            elapsed = time.monotonic() - start
            if elapsed >= next_sample:
                samples.append(resource_snapshot(database, server))
                print(f"PostgreSQL load: {int(elapsed)}s, unique={unique}, duplicate_requests={duplicates}, queries={queries}, connections={samples[-1]['connections']}", file=sys.stderr, flush=True)
                next_sample += 60
            time.sleep(max(0, 1 - (time.monotonic() - tick_start)))
    wait_event(http, project_id, final_event)
    until = time.monotonic() + 30
    while psql(database, "SELECT count(*) FROM metric.error_events WHERE processing_state=0") != "0":
        if time.monotonic() >= until:
            raise RuntimeError("soak left an undrained accepted Event backlog")
        time.sleep(0.2)
    rows = int(psql(database, "SELECT count(*) FROM metric.error_events"))
    # Scheduler has a bounded phase per table; a short diagnostic can end before
    # its next expiry pass. Do not assert synchronous deletion at the write edge.
    until = time.monotonic() + 60
    while rows == baseline_rows + unique and time.monotonic() < until:
        time.sleep(0.2)
        rows = int(psql(database, "SELECT count(*) FROM metric.error_events"))
    if rows != baseline_rows + unique - 1:
        expiry = psql(database, f"SELECT coalesce(expire_at::text,'NULL') FROM metric.error_events WHERE project_id={project_id} AND event_id=decode('{event_id}','hex')")
        raise RuntimeError(f"soak row gate: actual={rows}, expected={baseline_rows + unique - 1}, unique={unique}, baseline={baseline_rows}, synthetic_expiry={expiry or 'removed'}")
    issue_counts = int(psql(database, "SELECT coalesce(sum(occurrence_count),0) FROM metric.issues"))
    if issue_counts != baseline_rows + unique:
        raise RuntimeError("soak duplicated/missed authoritative Issue occurrence counts")
    samples.append(resource_snapshot(database, server))
    before_vacuum = time.monotonic()
    psql(database, "VACUUM (ANALYZE)")
    vacuum_seconds = time.monotonic() - before_vacuum
    def percentile(values, p):
        ordered = sorted(values)
        return round(ordered[min(len(ordered)-1, int((len(ordered)-1)*p))] * 1000, 3)
    report = {"suite": "load", "passed": 5, "target_seconds": duration,
        "elapsed_seconds": round(time.monotonic()-start, 3), "acceptance_30_minutes": duration >= 1800,
        "unique_events": unique, "duplicate_requests": duplicates, "concurrent_queries": queries,
        "write_batch_p95_ms": percentile(latencies, .95), "write_batch_p99_ms": percentile(latencies, .99),
        "query_p95_ms": percentile(query_latencies, .95), "query_p99_ms": percentile(query_latencies, .99),
        "workers": 8, "pool_max": 10, "burst_events": 80, "steady_events_per_tick": 8,
        "vacuum_seconds": round(vacuum_seconds, 3), "samples": samples}
    path = ROOT / "target" / f"postgres-soak-{uuid.uuid4().hex}.json"
    path.write_text(json.dumps(report, indent=2), encoding="utf-8")
    # Keep ordinary output concise; the artifact has only bounded resource metadata.
    return {k: v for k, v in report.items() if k != "samples"} | {"report": str(path)}


def build_image():
    # Reuse the shipped Dockerfile and all its stages. Only serialize Cargo for
    # the measured developer fixture; production build policy stays unchanged.
    source = (ROOT / "Dockerfile").read_text(encoding="utf-8")
    needle = "cargo build --locked --release --bin metric-server"
    if source.count(needle) != 1:
        raise RuntimeError("Dockerfile build shape changed; review fixture serialization")
    path = ROOT / "target/postgres-test.Dockerfile"
    path.parent.mkdir(exist_ok=True)
    path.write_text(source.replace(needle, "cargo build -j1 --locked --release --bin metric-server"), encoding="utf-8")
    run(["docker", "build", "--file", str(path), "--tag", "metric-postgres:local", "."], timeout=3600)


def container_suite(args, *, offline=False, tls=False):
    run(["docker", "image", "inspect", "metric-postgres:local", "--format", "{{.Id}}"], capture=True)
    project_name = "metric-pg-app-" + uuid.uuid4().hex
    assert re.fullmatch(r"metric-pg-app-[0-9a-f]{32}", project_name)
    temp_root = ROOT / "target/postgres-container-tests"
    temp_root.mkdir(parents=True, exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix="container-", dir=temp_root))
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    db_service = "postgres-tls" if tls else "postgres"
    override = directory / "override.yml"
    # !override prevents retaining the developer fixture's fixed published ports.
    content = "services:\n  " + db_service + ":\n    ports: !override []\n"
    content += "    environment:\n      POSTGRES_DB: metric_app_test\n"
    content += "    healthcheck:\n      test: [CMD-SHELL, 'pg_isready -U metric_test -d metric_app_test']\n"
    content += f"  metric-postgres:\n    ports: !override ['127.0.0.1:{port}:4001']\n"
    if not tls:
        content += "    environment:\n      METRIC_POSTGRES_TEST_URI: postgres://metric_test:metric-postgres-local-only@postgres:5432/metric_app_test\n"
    if tls:
        content += "    depends_on: !override\n      postgres-tls:\n        condition: service_healthy\n"
        content += "    environment:\n      METRIC_POSTGRES_TEST_URI: postgres://metric_test:metric-postgres-local-only@postgres-tls:5432/metric_app_test\n"
        content += "      APP__POSTGRES__SSL_MODE: verify-full\n      APP__POSTGRES__ALLOW_INSECURE_CONNECTION: 'false'\n      APP__POSTGRES__SSL_ROOT_CERT: /run/metric-test-tls/ca.crt\n"
        content += "    volumes:\n      - postgres-test-certs:/run/metric-test-tls:ro\n"
    override.write_text(content, encoding="utf-8")
    compose = ["docker", "compose", "-p", project_name, "-f", str(ROOT / "deploy/compose.postgres.dev.yml"), "-f", str(override)]
    if offline:
        compose.extend(["-f", str(ROOT / "deploy/compose.postgres.offline.yml")])
    compose.extend(["--profile", "app"])
    if tls:
        compose.extend(["--profile", "tls"])
    app_id = None
    try:
        run(compose + ["config", "--quiet"], capture=True)
        # Images must already be provisioned: runtime tests never pull/install.
        run(compose + ["up", "-d", "--wait", "--wait-timeout", "120", "--no-build", "--pull", "never", "metric-postgres"], capture=True, timeout=180)
        app_id = run(compose + ["ps", "-q", "metric-postgres"], capture=True).strip()
        db_id = run(compose + ["ps", "-q", db_service], capture=True).strip()
        if not app_id or not db_id:
            raise RuntimeError("expected composed PostgreSQL/app containers did not start")
        services = run(compose + ["ps", "--services"], capture=True).splitlines()
        if "mongodb" in services or "mongo" in services:
            raise RuntimeError("PostgreSQL fixture unexpectedly depends on Mongo")
        if offline:
            networks = json.loads(run(["docker", "inspect", app_id, "--format", "{{json .NetworkSettings.Networks}}"], capture=True))
            if not networks:
                raise RuntimeError("offline fixture has no native DB network")
            for network in networks.values():
                internal = run(["docker", "network", "inspect", network["NetworkID"], "--format", "{{.Internal}}"], capture=True).strip()
                if internal != "true":
                    raise RuntimeError("offline fixture permits outbound networking")
        # Docker Desktop may not expose ports from an internal-only network.
        # Run API probes on container loopback while DB traffic stays isolated.
        http = ContainerHttp(app_id) if offline else Http(f"http://127.0.0.1:{port}")
        if not http.ready():
            raise RuntimeError("container was healthy but its readiness is closed")
        logs = run(["docker", "logs", app_id], capture=True, combine=True)
        token = re.search(r"METRIC_BOOTSTRAP_TOKEN=([0-9a-f]+)", logs)
        if not token:
            raise RuntimeError("synthetic container did not emit setup token")
        project, event, sdk = exercise_http(http, token[1], args.sdk and not offline, args.browser and not offline)
        run(["docker", "exec", app_id, "metric-server", "--config", "/etc/metric/postgres-test.toml", "--check-config"], capture=True)
        run(compose + ["restart", "--no-deps", db_service], capture=True, timeout=60)
        until = time.monotonic() + 60
        while not http.ready():
            if time.monotonic() >= until:
                raise RuntimeError("readiness did not recover after native DB restart")
            time.sleep(0.2)
        wait_event(http, project["project_id"], event["event_id"])
        before_stop = time.monotonic()
        run(compose + ["stop", "-t", "15", "metric-postgres"], capture=True, timeout=30)
        exit_code = run(["docker", "inspect", app_id, "--format", "{{.State.ExitCode}}"], capture=True).strip()
        if exit_code != "0":
            raise RuntimeError("container failed graceful SIGTERM shutdown")
        elapsed = time.monotonic() - before_stop
        run(["docker", "start", app_id], capture=True, timeout=30)
        until = time.monotonic() + 60
        while not http.ready():
            if time.monotonic() >= until:
                raise RuntimeError("container readiness did not recover after restart")
            time.sleep(0.2)
        if wait_event(http, project["project_id"], event["event_id"]) != event:
            raise RuntimeError("container/native DB restart changed acknowledged Event")
        return {"suite": "offline" if offline else "container", "passed": 12,
                "transport": "verify-full" if tls else "disable-acknowledged", "sdk": sdk,
                "shutdown_seconds": round(elapsed, 3)}
    finally:
        # Never use global prune. Verify exact generated project ownership before
        # removing its disposable volumes. Keep them on any identity mismatch.
        volume_names = run(["docker", "volume", "ls", "--filter", f"label=com.docker.compose.project={project_name}", "--format", "{{.Name}}"], capture=True).splitlines()
        for name in volume_names:
            label = run(["docker", "volume", "inspect", name, "--format", '{{index .Labels "com.docker.compose.project"}}'], capture=True).strip()
            if label != project_name or not name.startswith(project_name + "_"):
                raise RuntimeError("fixture volume ownership mismatch; volumes preserved")
        run(compose + ["down", "--volumes", "--remove-orphans"], capture=True, timeout=90)
        resolved = directory.resolve()
        if resolved.parent == temp_root.resolve() and resolved.name.startswith("container-"):
            shutil.rmtree(resolved)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--suite", required=True, choices=("adapter", "server", "startup", "security", "migrations", "faults", "container", "offline", "load"))
    parser.add_argument("--binary", help="already-built server binary; avoids a nested Cargo build")
    parser.add_argument("--backend", default="postgres", choices=("postgres", "mongodb"), help="real server functional gates may also select the independent Mongo fixture")
    parser.add_argument("--verify-purge", action="store_true", help="server suite: verify real project deletion and zero remaining owned records after readback/restart")
    parser.add_argument("--verify-backup", action="store_true", help="server suite: dump and restore the owned database before restart/readback")
    parser.add_argument("--sdk", action="store_true", help="require installed real Node/Python/Java/.NET/Go/Rust/Ruby/PHP SDK fixtures")
    parser.add_argument("--browser", action="store_true", help="require installed Playwright and built Web/browser SDK fixtures")
    parser.add_argument("--build-image", action="store_true", help="build the current source image with serialized Cargo before container gates")
    parser.add_argument("--soak-duration", default="30m", help="bounded load interval; short diagnostics do not satisfy the 30 minute gate")
    args = parser.parse_args()
    if args.verify_backup and args.suite != "server":
        raise RuntimeError("backup verification is a functional server gate")
    if args.verify_purge and args.suite != "server":
        raise RuntimeError("purge verification is a functional server gate")
    if args.backend == "mongodb":
        if args.suite != "server":
            raise RuntimeError("Mongo selection supports only the functional server suite")
        run(["docker", "info", "--format", "{{.ServerVersion}}"], capture=True, timeout=20)
        if run(["docker", "inspect", "--format", "{{.State.Health.Status}}", MONGO_CONTAINER], capture=True).strip() != "healthy":
            raise RuntimeError("isolated Mongo fixture must be healthy; no silent skip")
    else:
        require_docker(args.suite not in ("container", "offline"))
    if args.build_image:
        build_image()
    if args.suite in ("server", "load"):
        result = server_suite(args)
    elif args.suite == "startup":
        result = startup_suite(args)
    elif args.suite == "container":
        result = container_suite(args)
    elif args.suite == "offline":
        plain = container_suite(args, offline=True)
        tls = container_suite(args, offline=True, tls=True)
        result = {"suite": "offline", "passed": plain["passed"] + tls["passed"], "transports": [plain, tls]}
    else:
        result = adapter_suite(args.suite)
    if not result["passed"]:
        raise RuntimeError("zero acceptance cases ran")
    print(json.dumps(result))


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"Storage acceptance failed: {error}", file=sys.stderr)
        sys.exit(1)
