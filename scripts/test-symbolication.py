#!/usr/bin/env python3
"""Real pinned Symbolicator -> private callbacks -> finalized Metric events.

Uses synthetic, invocation-owned resources. --keep leaves a local demo running.
The default run fails on missing infrastructure or zero cases; it never skips.
"""
from __future__ import annotations

import argparse
import datetime as dt
import importlib.util
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from http.server import SimpleHTTPRequestHandler
from functools import partial
import json
import os
import re
import shutil
from pathlib import Path
import socket
import subprocess
import sys
import threading
import time
import tomllib
import uuid
import urllib.error
import urllib.request
from urllib.parse import parse_qs, urlsplit

sys.dont_write_bytecode = True

ROOT = Path(__file__).resolve().parents[1]
IMAGE = "ghcr.io/getsentry/symbolicator:26.6.0@sha256:b66d1d5c796e056bf19f597bb7d1ac39604a767eac501dee3710b8844b12228f"
spec = importlib.util.spec_from_file_location("postgres_fixture", ROOT / "scripts/test-postgres.py")
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


def port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


class Recorder:
    """Pass bytes to real services and retain only sanitized contract evidence."""
    def __init__(self, upstream, *, callbacks=False):
        self.requests = []
        self.credentials = {}
        self.failure_status = None
        recorder = self
        records = self.requests
        credentials = self.credentials
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        class Handler(BaseHTTPRequestHandler):
            def forward(self):
                body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
                if recorder.failure_status:
                    records.append({"path": self.path, "status": recorder.failure_status, "fault_injected": True})
                    self.send_response(recorder.failure_status)
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                    return
                headers = {name: self.headers[name] for name in ("Content-Type", "Authorization") if name in self.headers}
                request = urllib.request.Request(upstream + self.path, data=body if body else None, headers=headers, method=self.command)
                try:
                    response = opener.open(request, timeout=35)
                except urllib.error.HTTPError as error:
                    response = error
                with response:
                    data = response.read(16 * 1024 * 1024 + 1)
                    record = {"path": self.path, "status": response.code}
                    if not callbacks:
                        payload = json.loads(body)
                        for source in payload.get("sources", []) + ([payload["source"]] if "source" in payload else []):
                            credentials[source["url"]] = source["token"]
                            source["token"] = "[redacted]"
                        record.update(request=payload, response=json.loads(data) if response.headers.get("Content-Type", "").startswith("application/json") else data.decode(errors="replace"))
                    records.append(record)
                    self.send_response(response.code)
                    self.send_header("Content-Type", response.headers.get("Content-Type", "application/octet-stream"))
                    self.send_header("Content-Length", str(len(data)))
                    self.end_headers()
                    self.wfile.write(data)

            do_POST = forward
            do_GET = forward

            def log_message(self, *_args):
                pass
        self.server = ThreadingHTTPServer(("0.0.0.0" if callbacks else "127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    @property
    def port(self):
        return self.server.server_port

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)


class Suite:
    def __init__(self, args):
        self.args = args
        self.id = uuid.uuid4().hex
        self.directory = ROOT / "target/symbolication-tests" / self.id
        self.directory.mkdir(parents=True)
        self.database = ("metric_pg_runtime_" if args.backend == "postgres" else "metric_mongo_runtime_") + self.id
        self.container = "metric-symbolication-" + self.id
        self.http = fixture.Http(f"http://127.0.0.1:{args.port or port()}")
        self.symbolicator_port = port()
        self.server = None
        self.started_container = False
        self.created_database = False
        self.cases = []
        self.events = []
        self.secrets = []
        self.recorders = []

    def run(self):
        fixture.run(["docker", "info", "--format", "{{.ServerVersion}}"], capture=True, timeout=20)
        if self.args.backend == "postgres":
            fixture.require_docker()
            fixture.psql("metric_test", f"CREATE DATABASE {self.database}")
        else:
            require(fixture.mongo(self.database, "print(fixture.getCollectionNames().length)") == "0", "nonempty fixture database")
        self.created_database = True
        config = self.directory / "symbolicator.yml"
        config.write_text("cache_dir: /data\nbind: 0.0.0.0:3021\nconnect_to_reserved_ips: false\nlogging:\n  level: warn\n", encoding="utf-8")
        fixture.run(["docker", "run", "--rm", "-d", "--name", self.container,
            "--add-host", "host.docker.internal:host-gateway", "-p", f"127.0.0.1:{self.symbolicator_port}:3021",
            "--mount", f"type=bind,source={config},target=/etc/symbolicator.yml,readonly",
            IMAGE, "run", "--config", "/etc/symbolicator.yml"], capture=True, timeout=60)
        self.started_container = True
        symbolicator = fixture.Http(f"http://127.0.0.1:{self.symbolicator_port}")
        until = time.monotonic() + 30
        while True:
            try:
                symbolicator.request("/healthcheck")
                break
            except (OSError, RuntimeError):
                require(time.monotonic() < until, "Symbolicator readiness deadline exceeded")
                time.sleep(0.1)
        binary = Path(self.args.binary).resolve()
        require(binary.is_file(), "build the Metric binary first")
        if self.args.keep:
            # Windows locks a running executable. Leave Cargo's build output free
            # for subsequent checks and development while the demo is running.
            snapshot = self.directory / binary.name
            shutil.copy2(binary, snapshot)
            binary = snapshot
        self.backend_recorder = Recorder(symbolicator.base)
        self.callback_recorder = Recorder(self.http.base, callbacks=True)
        self.recorders = [self.backend_recorder, self.callback_recorder]
        env = {key: value for key, value in os.environ.items() if not key.startswith("APP__")}
        env.update(METRIC_POSTGRES_TEST_URI=fixture.ADMIN_URI.rsplit("/", 1)[0] + "/" + self.database,
                   METRIC_POSTGRES_TEST_HMAC=fixture.HMAC,
                   METRIC_MONGO_TEST_URI="mongodb://metric:metric-local-only@127.0.0.1:27018/?authSource=admin")
        source = (ROOT / "deploy/metric.postgres.test.toml").read_text(encoding="utf-8")
        source = source.replace('backend = "postgres"', f'backend = "{self.args.backend}"', 1)
        # Docker must reach the host listener for private callbacks. Published UI uses localhost.
        source = source.replace('http_address = "0.0.0.0:4001"', f'http_address = "0.0.0.0:{self.http.base.rsplit(":", 1)[1]}"')
        source = source.replace('root = "/var/lib/metric/blobs"', f'root = "{self.directory.as_posix()}/blobs"')
        source += (f'\n[symbolicator]\nendpoint="http://127.0.0.1:{self.backend_recorder.port}/symbolicate"\n'
                   f'callback_base_url="http://host.docker.internal:{self.callback_recorder.port}/"\n'
                   'request_timeout="20s"\ncircuit_failure_threshold=1000\n\n[processor]\nstage_timeout="30s"\ntotal_timeout="60s"\nretry_base="100ms"\nretry_max="1s"\n')
        if self.args.backend == "mongodb":
            source += f'\n[mongodb]\nuri={{env="METRIC_MONGO_TEST_URI"}}\ndatabase="{self.database}"\n'
        server_config = self.directory / "metric.toml"
        server_config.write_text(source, encoding="utf-8")
        self.server = fixture.NativeServer(binary, server_config, env)
        self.server.start()
        self.server.wait_ready(self.http)
        require(self.server.token, "empty test installation did not emit bootstrap token")
        self.bootstrap()
        self.exercise()
        required = {"native-breakpad-cold", "native-elf-dwarf", "native-real-pdb", "native-pdb-source-bundle", "javascript-source-map", "native-missing", "native-project-isolation", "private-callback-lookup-and-download"}
        require(required.issubset(self.cases), "required real symbolication cases did not execute")

    def bootstrap(self):
        self.email = "symbolication@example.invalid"
        self.password = "local symbolication synthetic password"
        context = self.http.request("/api/v1/auth/bootstrap", {"setup_token": self.server.token,
            "email": self.email, "display_name": "Symbolication Demo", "password": self.password,
            "organization_slug": "symbols", "organization_name": "Local Symbolication"})
        self.organization_id = context["organization_id"]
        login = self.http.request("/api/v1/auth/login", {"email": self.email, "password": self.password,
            "organization_id": self.organization_id})
        self.http.headers.update({"x-metric-organization-id": self.organization_id, "x-csrf-token": login["csrf_token"]})
        self.project = self.http.request("/api/v1/projects", {"slug": "symbolication", "display_name": "Symbolication Demo"}, expected=201)
        self.other = self.http.request("/api/v1/projects", {"slug": "isolated", "display_name": "Isolated Symbols"}, expected=201)
        self.project["slug"] = "symbolication"
        self.other["slug"] = "isolated"
        expires = (dt.datetime.now(dt.timezone.utc) + dt.timedelta(days=1)).isoformat()
        self.token = self.http.request("/api/v1/auth/tokens", {"name": "symbolication-test",
            "scopes": ["debug_file:write", "artifact:write"], "expires_at": expires}, expected=201)["token"]
        self.secrets.extend([self.server.token, self.token, self.project["dsn_key"], self.other["dsn_key"]])

    def cli(self, project, *arguments):
        env = os.environ.copy()
        env.update(SENTRY_URL=self.http.base + "/", SENTRY_AUTH_TOKEN=self.token,
                   SENTRY_ORG="symbols", SENTRY_PROJECT=project["slug"],
                   SENTRY_DISABLE_UPDATE_CHECK="1", SENTRY_NO_PROGRESS_BAR="1")
        return fixture.run(["node", str(ROOT / "sdk-tests/sentry-cli/node_modules/@sentry/cli/bin/sentry-cli"),
                            *arguments], env=env, capture=True, timeout=90)

    def upload_native(self, project, path):
        self.cli(project, "debug-files", "upload", str(path))

    def event(self, label, *, project=None, frames=None, images=None, platform="native", body=None, expected="complete"):
        project = project or self.project
        event_id = uuid.uuid4().hex
        payload = {"event_id": event_id, "platform": platform, "timestamp": time.time(),
            "level": "error", "environment": "symbolication-local", "release": "symbolication-demo@1",
            "dist": "test", "fingerprint": ["symbolication", label],
            "exception": {"values": [{"type": label, "value": "Local real Symbolicator contract",
                "stacktrace": {"frames": frames or []}}]}, "debug_meta": {"images": images or []}}
        payload.update(body or {})
        self.http.request(f"/api/{project['project_id']}/store/?sentry_key={project['dsn_key']}", payload)
        event = fixture.wait_event(self.http, project["project_id"], event_id, label)
        result = event["body"]["_metric"]["symbolication"]
        self.events.append({"label": label, "project_id": project["project_id"], "event_id": event_id,
                            "issue_id": event["issue_id"], "symbolication": result})
        require(result["status"] == expected, f"{label}: expected {expected}, got {json.dumps(result)}")
        require(event["body"].get("exception") == payload["exception"], f"{label}: original exception stack changed")
        return result

    def native(self, label, debug_id, *, project=None, expected="complete", address="0x10001000"):
        return self.event(label, project=project, expected=expected,
            frames=[{"instruction_addr": address, "in_app": True}],
            images=[{"type": "pe", "debug_id": debug_id, "code_file": "metric.pdb",
                     "image_addr": "0x10000000", "image_size": 8192}])

    def check_frame(self, result, function, filename, line):
        frame = result["derived"][0]["frames"][0]
        require(frame["function"] == function, f"unexpected function: {frame}")
        require(frame["filename"].replace("\\", "/").endswith(filename), f"unexpected source: {frame}")
        require(frame["line"] == line, f"unexpected source line: {frame}")
        return frame

    def case(self, name):
        self.cases.append(name)
        print("PASS " + name, flush=True)

    def check_source_stack(self, result, functions, source, *, subset=False):
        frames = result["derived"][0]["frames"]
        require([frame["function"] for frame in frames] == functions, f"caller order or inline frames changed: {frames}")
        if not subset:
            require([frame["original_index"] for frame in frames] == list(range(len(functions))), "original frame identity changed")
        lines = source.read_text(encoding="utf-8").splitlines()
        for frame in frames:
            line = frame["line"]
            require(frame["filename"].replace("\\", "/").endswith(source.name), f"wrong source: {frame}")
            require(frame.get("context_line") == lines[line - 1], f"wrong current source line: {frame}")
            require(frame.get("pre_context", []) == lines[max(0, line - 6):line - 1], f"wrong pre-context: {frame}")
            require(frame.get("post_context", []) == lines[line:line + 5], f"wrong post-context: {frame}")

    def pdb(self):
        directory = self.directory / "pdb"
        directory.mkdir()
        source = directory / "native-pdb.c"
        shutil.copyfile(ROOT / "sdk-tests/symbolicator/fixtures/native-pdb.c", source)
        obj, binary, pdb, map_file = [directory / name for name in ("native.obj", "metric-pdb.exe", "metric-pdb.pdb", "metric-pdb.map")]
        fixture.run(["clang", "--target=x86_64-pc-windows-msvc", "-g", "-gcodeview", "-O0", "-c", str(source), "-o", str(obj)], capture=True)
        fixture.run(["lld-link", "/debug:full", "/nodefaultlib", "/entry:main", "/subsystem:console",
                     "/out:" + str(binary), "/pdb:" + str(pdb), "/map:" + str(map_file), str(obj)], capture=True)
        checked = self.cli(self.project, "debug-files", "check", str(pdb))
        require("Type: pdb" in checked and "Usable: yes" in checked, "compiler did not produce a usable real PDB")
        debug_id = re.search(r"(?i)debug id:\s*([0-9a-f-]+)", checked).group(1)
        image_address = int(re.search(r"Preferred load address is ([0-9a-fA-F]+)", map_file.read_text()).group(1), 16)
        first_function = int(re.search(r"metric_pdb_fail\s+([0-9a-fA-F]+)", map_file.read_text()).group(1), 16)
        line_records = fixture.run(["llvm-pdbutil", "dump", "-l", str(pdb)], capture=True)
        offsets = {int(line): int(offset, 16) for line, offset in re.findall(r"\b(\d+)\s+([0-9A-Fa-f]{8})\s+!", line_records)}
        source_lines = source.read_text().splitlines()
        markers = ["int response = metric_pdb_parse(input);", "int parsed = metric_pdb_fail(normalized);", "int result = numerator / denominator;"]
        frames = []
        for marker in markers:
            line = next(index for index, text in enumerate(source_lines, 1) if marker in text)
            # An address within the line works for both instruction and return-address adjustment.
            frames.append({"instruction_addr": hex(first_function + offsets[line] + 1), "in_app": True})
        images = [{"type": "pe", "debug_id": debug_id, "code_file": binary.name,
                   "image_addr": hex(image_address), "image_size": 0x100000}]
        self.upload_native(self.project, pdb)
        result = self.event("NativePdbWithoutSources", frames=frames, images=images)
        expected = ["metric_pdb_handle(int)", "metric_pdb_parse(int)", "metric_pdb_fail(int)"]
        require([frame["function"] for frame in result["derived"][0]["frames"]] == expected, "real PDB did not resolve every caller")
        require(all("context_line" not in frame for frame in result["derived"][0]["frames"]), "PDB-only fixture unexpectedly contained sources")
        self.case("native-real-pdb")
        self.cli(self.project, "debug-files", "upload", "--include-sources", str(pdb))
        source_url, token = next((url, token) for url, token in self.backend_recorder.credentials.items()
                                 if f"/projects/{self.project['project_id']}/debug-files/" in url)
        private = fixture.Http(self.http.base)
        private.headers["Authorization"] = "Bearer " + token
        files = private.request(urlsplit(source_url).path + "?debug_id=" + debug_id)
        require(any(file["symbolType"] == "sourcebundle" for file in files), "CLI did not publish a native source bundle")
        result = self.event("NativePdbSourceContext", frames=frames, images=images)
        self.check_source_stack(result, expected, source)
        self.case("native-pdb-source-bundle")
        self.check_source_stack(self.event("NativePdbSourceWarm", frames=frames, images=images), expected, source)
        self.case("native-pdb-source-context-warm")
        # PDB debug information alone must not acquire another project's source bundle.
        self.upload_native(self.other, pdb)
        foreign = self.event("NativePdbIsolated", project=self.other, frames=frames, images=images)
        require(all("context_line" not in frame for frame in foreign["derived"][0]["frames"]), "native sources crossed the project boundary")
        self.case("native-source-bundle-project-isolation")

    def exercise(self):
        path = ROOT / "sdk-tests/sentry-cli/fixtures/metric.sym"
        debug_id = "67e9247c-814e-392b-a027-dbde6748fcbf-1"
        self.upload_native(self.project, path)
        result = self.native("NativeBreakpad", debug_id)
        self.check_frame(result, "metric_sdk_compatibility", "src/main.rs", 42)
        self.case("native-breakpad-cold")
        self.check_frame(self.native("NativeWarm", debug_id), "metric_sdk_compatibility", "src/main.rs", 42)
        self.case("native-breakpad-warm")
        missing_id = str(uuid.uuid4()) + "-1"
        missing = self.native("NativeMissing", missing_id, expected="missing")
        require(missing["missing_debug_ids"] == [missing_id], "native missing Debug ID was lost")
        self.case("native-missing")
        # Same Debug ID in another project must never hit the first project's cache.
        isolated = self.directory / "isolated.sym"
        isolated.write_text(path.read_text().replace("metric_sdk_compatibility", "isolated_native_function").replace("42 0", "84 0"), encoding="utf-8")
        self.upload_native(self.other, isolated)
        self.check_frame(self.native("NativeIsolated", debug_id, project=self.other), "isolated_native_function", "src/main.rs", 84)
        self.case("native-project-isolation")
        late_id = str(uuid.uuid4()) + "-1"
        self.native("NativeBeforeUpload", late_id, expected="missing")
        late = self.directory / "late.sym"
        late.write_text(path.read_text().replace("67E9247C814E392BA027DBDE6748FCBF1", late_id.replace("-", "").upper()), encoding="utf-8")
        self.upload_native(self.project, late)
        self.check_frame(self.native("NativeAfterUpload", late_id), "metric_sdk_compatibility", "src/main.rs", 42)
        self.case("native-negative-cache-revision")
        images = [{"type": "pe", "debug_id": debug_id, "code_file": "metric.pdb", "image_addr": "0x10000000", "image_size": 8192}]
        result = self.event("NativePartial", frames=[{"instruction_addr": "0x10001001"}, {"instruction_addr": "0x20001000"}], images=images, expected="partial")
        require([frame["original_index"] for frame in result["derived"][0]["frames"]] == [0], "partial native frame identity changed")
        self.case("native-partial")
        result = self.event("NativeMultipleTraces", frames=[{"instruction_addr": "0x10001001"}], images=images,
            body={"stacktrace": {"frames": [{"instruction_addr": "0x10001002"}]},
                  "exception": {"values": [{"type": "NativeMultipleTraces", "stacktrace": {"frames": [{"instruction_addr": "0x10001001"}]},
                    "raw_stacktrace": {"frames": [{"instruction_addr": "0x10001003"}]}}]}})
        require([trace["origin"] for trace in result["derived"]] == [{"kind": "event"}, {"kind": "exception", "index": 0}, {"kind": "exception_raw", "index": 0}], "trace origins changed")
        for trace in result["derived"]:
            require(trace["frames"][0]["function"] == "metric_sdk_compatibility" and trace["frames"][0]["line"] == 42, "multi trace symbol mismatch")
        self.case("native-multiple-traces")
        self.pdb()
        js_directory = self.directory / "javascript"
        js_directory.mkdir()
        js = js_directory / "app.min.js"
        self.build_js(ROOT / "sdk-tests/symbolicator/fixtures/source-map.js", js, "cjs", "node")
        self.cli(self.project, "sourcemaps", "inject", str(js_directory))
        debug_id_js = json.loads(js.with_suffix(".js.map").read_text())["debugId"]
        # Capture real generated positions from three calls, without source-map support.
        driver = "try { require(process.argv[1]).metricSourceMapHandle(); } catch (error) { process.stdout.write(error.stack); }"
        stack = fixture.run(["node", "-e", driver, str(js)], capture=True)
        positions = re.findall(re.escape(str(js)) + r":(\d+):(\d+)", stack)
        require(len(positions) == 3, "expected three real JavaScript application calls")
        js_frames = [{"filename": "app.min.js", "abs_path": "https://example.invalid/static/app.min.js",
                      "lineno": int(line), "colno": int(column)} for line, column in reversed(positions)]
        js_images = [{"type": "sourcemap", "debug_id": debug_id_js, "code_file": "https://example.invalid/static/app.min.js"}]
        self.event("JavaScriptBeforeUpload", platform="javascript", frames=js_frames, images=js_images,
            body={"release": None, "dist": None}, expected="missing")
        self.cli(self.project, "sourcemaps", "upload", "--release", "symbolication-demo@1", "--dist", "test",
                 "--url-prefix", "~/static", str(js_directory))
        result = self.event("JavaScriptSourceMap", platform="javascript",
            frames=js_frames, images=js_images)
        expected_js = ["metricSourceMapHandle", "metricSourceMapParse", "metricSourceMapFailure"]
        self.check_source_stack(result, expected_js, ROOT / "sdk-tests/symbolicator/fixtures/source-map.js")
        require(result["derived"][0]["frames"][-1]["column"] == 9, "JavaScript source column changed")
        self.case("javascript-source-map")
        self.case("javascript-negative-cache-revision")
        gap = self.event("JavaScriptGapSourceContext", platform="javascript", expected="partial",
            frames=[js_frames[0], {"function": "native_without_location", "in_app": False}, *js_frames[1:]],
            images=js_images)
        gap_frames = gap["derived"][0]["frames"]
        require([frame["original_index"] for frame in gap_frames] == [0, 2, 3], "JS gap changed original frame indexes")
        for actual, original in zip(gap_frames, result["derived"][0]["frames"]):
            for field in ("filename", "line", "column", "pre_context", "context_line", "post_context"):
                require(actual.get(field) == original.get(field), f"JS gap changed {field}")
        self.case("javascript-gap-preserves-source-context")
        self.event("JavaScriptUnsendable", platform="javascript", expected="missing",
            frames=[{"function": "native_without_location"}], images=js_images)
        self.case("javascript-unsendable-stays-raw")
        self.check_source_stack(self.event("JavaScriptWarm", platform="javascript", frames=js_frames, images=js_images),
                                expected_js, ROOT / "sdk-tests/symbolicator/fixtures/source-map.js")
        self.case("javascript-warm")
        self.check_source_stack(self.event("NodeReleaseSourceMap", platform="node", frames=js_frames, images=[]),
                                expected_js, ROOT / "sdk-tests/symbolicator/fixtures/source-map.js")
        self.case("node-release-dist-source-map")
        foreign_js = self.directory / "isolated-js"
        shutil.copytree(js_directory, foreign_js)
        foreign_map = foreign_js / "app.min.js.map"
        foreign_map.write_text(foreign_map.read_text().replace("metricSourceMapFailure", "isolatedSourceMapFailure"), encoding="utf-8")
        self.cli(self.other, "sourcemaps", "upload", "--release", "symbolication-demo@1", "--dist", "test", "--url-prefix", "~/static", str(foreign_js))
        isolated_result = self.event("JavaScriptIsolated", project=self.other, platform="javascript", frames=js_frames, images=js_images)
        require(isolated_result["derived"][0]["frames"][-1]["function"] == "isolatedSourceMapFailure", "foreign source-map cache crossed project boundary")
        self.case("javascript-project-isolation")
        self.event("JavaScriptMissing", platform="javascript", expected="missing", body={"release": None, "dist": None},
            frames=[{"function": "stillRaw", "filename": "missing.min.js", "abs_path": "https://example.invalid/missing.min.js", "lineno": 1, "colno": 2}],
            images=[{"type": "sourcemap", "debug_id": str(uuid.uuid4()), "code_file": "https://example.invalid/missing.min.js"}])
        self.case("javascript-missing-is-not-success")
        self.event("ManagedStack", platform="python", expected="not_required",
            frames=[{"function": "ready_function", "filename": "ready.py", "lineno": 17}], images=[])
        self.case("ready-stack-preserved")
        self.elf()
        for kind in ("debug-files", "artifact-lookup"):
            hits = [row for row in self.callback_recorder.requests if kind in row["path"] and row["status"] == 200]
            require(any("id" not in parse_qs(urlsplit(row["path"]).query) for row in hits), f"missing real {kind} lookup")
            require(any("&id=" in row["path"] or "?id=" in row["path"] for row in hits), f"missing real {kind} download")
        self.case("private-callback-lookup-and-download")
        self.private_isolation()
        if self.args.faults:
            self.backend_recorder.failure_status = 503
            try:
                self.native("BackendUnavailableRawFallback", debug_id, expected="unavailable")
            finally:
                self.backend_recorder.failure_status = None
            self.case("backend-unavailable-preserves-raw")
            self.check_frame(self.native("BackendRecovered", debug_id), "metric_sdk_compatibility", "src/main.rs", 42)
            self.case("backend-recovery")
        if self.args.sdk:
            self.sdk_symbolication()
            self.sdk_regression()
        if self.args.ui:
            self.verify_ui()

    def private_isolation(self):
        for url, token in self.backend_recorder.credentials.items():
            self.secrets.append(token)
            if f"/projects/{self.project['project_id']}/" not in url:
                continue
            client = fixture.Http(self.http.base)
            client.headers["Authorization"] = "Bearer " + token
            path = urlsplit(url).path
            foreign_path = path.replace(f"/projects/{self.project['project_id']}/", f"/projects/{self.other['project_id']}/")
            client.request(foreign_path + "?debug_id=" + str(uuid.uuid4()), expected=401)
            other_kind = path.replace("artifact-lookup", "debug-files") if "artifact-lookup" in path else path.replace("debug-files", "artifact-lookup")
            client.request(other_kind + "?debug_id=" + str(uuid.uuid4()), expected=401)
        foreign_download = next(row["path"] for row in self.callback_recorder.requests
            if f"/projects/{self.other['project_id']}/debug-files/" in row["path"] and "id" in parse_qs(urlsplit(row["path"]).query))
        private_source, token = next((url, value) for url, value in self.backend_recorder.credentials.items()
            if f"/projects/{self.project['project_id']}/debug-files/" in url)
        client = fixture.Http(self.http.base)
        client.headers["Authorization"] = "Bearer " + token
        client.request(foreign_download.replace(f"/projects/{self.other['project_id']}/", f"/projects/{self.project['project_id']}/"), expected=404)
        self.case("private-tokens-and-foreign-file-denied")

    def elf(self):
        source = self.directory / "native.c"
        source.write_text((ROOT / "sdk-tests/symbolicator/fixtures/native.c").read_text(), encoding="utf-8")
        binary = self.directory / "metric-native"
        def tool(name, *arguments):
            if os.name == "nt":
                converted = []
                for value in arguments:
                    if isinstance(value, Path):
                        value = fixture.run(["wsl", "-d", "Ubuntu", "--exec", "wslpath", "-a", str(value)], capture=True).strip()
                    converted.append(str(value))
                return fixture.run(["wsl", "-d", "Ubuntu", "--exec", name, *converted], capture=True, combine=True)
            return fixture.run([name, *map(str, arguments)], capture=True, combine=True)
        tool("gcc", "-g", "-O0", "-fno-pie", "-no-pie", "-Wl,--build-id=sha1", source, "-o", binary)
        symbols = tool("nm", "-n", binary)
        address = re.search(r"(?m)^([0-9a-f]+) T metric_native_contract$", symbols).group(1)
        load_addresses = re.findall(r"(?m)^\s*LOAD\s+0x[0-9a-f]+\s+(0x[0-9a-f]+)", tool("readelf", "-lW", binary))
        image_address = hex(min(int(value, 16) for value in load_addresses))
        checked = self.cli(self.project, "debug-files", "check", str(binary))
        debug_id = re.search(r"(?i)debug id:\s*([0-9a-f-]+)", checked).group(1)
        self.upload_native(self.project, binary)
        result = self.event("NativeElfDwarf", frames=[{"instruction_addr": "0x" + address}],
            images=[{"type": "elf", "debug_id": debug_id, "code_file": "metric-native", "image_addr": image_address, "image_size": 0x1000000}])
        self.check_frame(result, "metric_native_contract", "metric-symbolication.c", 42)
        self.case("native-elf-dwarf")

    def build_js(self, source, output, format, platform):
        # Resolve the existing build tool from its own locked package graph.
        driver = "const esbuild=require('./sdk-tests/browser/node_modules/esbuild'); esbuild.buildSync(JSON.parse(process.argv[1]));"
        fixture.run(["node", "-e", driver, json.dumps({"entryPoints": [str(source)], "outfile": str(output),
            "bundle": True, "minify": True, "sourcemap": "external", "sourcesContent": True,
            "format": format, "platform": platform})], capture=True)

    def dsn(self):
        return f"http://{self.project['dsn_key']}@{self.http.base.removeprefix('http://')}/{self.project['project_id']}"

    def sdk_result(self, label, output, *, symbols=False):
        reported = json.loads(output.strip().splitlines()[-1])
        require(reported.get("flushed"), f"{label}: SDK did not flush")
        event_id = uuid.UUID(reported["event_id"]).hex
        event = fixture.wait_event(self.http, self.project["project_id"], event_id, label)
        require(event["event_id"] == event_id and event["body"]["sdk"].get("version"), f"{label}: SDK identity lost")
        result = event["body"]["_metric"]["symbolication"]
        self.events.append({"label": label, "project_id": self.project["project_id"], "event_id": event_id,
                            "issue_id": event["issue_id"], "symbolication": result})
        if symbols:
            require(result["kind"] == "javascript" and result["status"] in ("complete", "partial"), f"{label}: source map did not resolve")
            frames = [frame for trace in result["derived"] for frame in trace["frames"]]
            frame = next((frame for frame in frames if frame.get("function") == "metricSourceMapFailure"), None)
            require(frame and frame["filename"].endswith("source-map.js") and frame["line"] == 10 and frame["column"] == 9, f"{label}: incorrect original function or position")
            application = [frame for frame in frames if frame.get("filename", "").endswith("source-map.js")]
            self.check_source_stack({"derived": [{"frames": application}]},
                                    ["metricSourceMapHandle", "metricSourceMapParse", "metricSourceMapFailure"],
                                    ROOT / "sdk-tests/symbolicator/fixtures/source-map.js", subset=True)
        return event

    def browser(self, directory, callback):
        class QuietHandler(SimpleHTTPRequestHandler):
            def log_message(self, *_args):
                pass
        server = ThreadingHTTPServer(("127.0.0.1", 0), partial(QuietHandler, directory=str(directory)))
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            return callback(f"http://127.0.0.1:{server.server_port}/")
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=2)

    def sdk_symbolication(self):
        for platform in ("node", "browser"):
            directory = self.directory / (platform + "-sdk")
            directory.mkdir()
            bundle = directory / ("app.cjs" if platform == "node" else "app.js")
            source = ROOT / ("sdk-tests/symbolicator/fixtures/source-map.js" if platform == "node" else "sdk-tests/browser/send-symbolication.js")
            self.build_js(source, bundle, "cjs" if platform == "node" else "iife", "node" if platform == "node" else "browser")
            self.cli(self.project, "sourcemaps", "inject", str(directory))
            self.cli(self.project, "sourcemaps", "upload", "--release", "symbolication-demo@1", "--url-prefix", "~/", str(directory))
            if platform == "node":
                output = fixture.run(["node", "sdk-tests/node/send-symbolication.mjs", self.dsn(), str(bundle)], capture=True, timeout=25)
            else:
                (directory / "index.html").write_text('<!doctype html><html><head><meta charset="utf-8"></head><body><script src="app.js"></script></body></html>', encoding="utf-8")
                output = self.browser(directory, lambda url: fixture.run(["node", "sdk-tests/browser/run-browser.mjs", url, self.dsn()], capture=True, timeout=35))
            self.sdk_result(platform + "SourceMapSDK", output, symbols=True)
            self.case(platform + "-real-sdk-source-map")

    def sdk_regression(self):
        dsn = self.dsn()
        classpath = os.pathsep.join([str(ROOT / "sdk-tests/java/.deps/sentry-8.50.1.jar"), str(ROOT / "sdk-tests/java/.deps/classes")])
        senders = {
            "javascript_node": (["node", "sdk-tests/node/send-error.mjs", dsn], ROOT),
            "python": ([sys.executable, "sdk-tests/python/send_error.py", dsn], ROOT),
            "java": (["java", "-cp", classpath, "MetricSdkCompatibility", dsn], ROOT),
            "dotnet": (["dotnet", "run", "--project", "sdk-tests/dotnet/MetricSdkCompatibility.csproj", "--configuration", "Release", "--no-build", "--no-restore", "--", dsn], ROOT),
            "go": (["go", "run", ".", dsn], ROOT / "sdk-tests/go"),
            "rust": (["cargo", "run", "--quiet", "--locked", "--manifest-path", "sdk-tests/rust/Cargo.toml", "--", dsn], ROOT),
            "ruby": (["ruby", "sdk-tests/ruby/send_error.rb", dsn], ROOT),
            "php": ([sys.executable, "sdk-tests/php/run.py", dsn], ROOT),
        }
        matrix = tomllib.loads((ROOT / "compatibility/sentry-sdk-matrix.toml").read_text())
        versions = {sdk["family"]: sdk["version"] for sdk in matrix["sdk"]}
        for family, (command, cwd) in senders.items():
            event = self.sdk_result(family, fixture.run(command, cwd=cwd, capture=True, timeout=180))
            require(event["body"]["sdk"]["version"] == versions[family], f"{family}: unexpected SDK version")
            self.case(family + "-sdk-regression")
        fixture.run([shutil.which("npm"), "run", "build"], cwd=ROOT / "sdk-tests/browser", capture=True)
        browser_root = self.directory / "browser-regression"
        browser_root.mkdir()
        shutil.copyfile(ROOT / "sdk-tests/browser/dist/send-error.js", browser_root / "sdk-browser.js")
        shutil.copyfile(ROOT / "sdk-tests/browser/page.html", browser_root / "index.html")
        output = self.browser(browser_root, lambda url: fixture.run(["node", "sdk-tests/browser/run-browser.mjs", url, dsn], capture=True, timeout=35))
        event = self.sdk_result("javascript_browser", output)
        require(event["body"]["sdk"]["version"] == versions["javascript_browser"], "browser: unexpected SDK version")
        self.case("javascript_browser-sdk-regression")

    def verify_ui(self):
        native_id = next(event["event_id"] for event in self.events if event["label"] == "NativePdbSourceContext")
        javascript_id = next(event["event_id"] for event in self.events if event["label"] == "JavaScriptSourceMap")
        output = fixture.run(["node", "sdk-tests/browser/verify-symbolication-ui.mjs", self.http.base,
            self.email, self.password, self.project["project_id"], native_id, javascript_id,
            str(self.directory / "native-event.png")], capture=True, timeout=90)
        require(json.loads(output)["passed"] == 2, "real UI did not verify both events")
        self.case("real-ui-native-and-javascript")

    def report(self, error=None):
        if self.recorders:
            self.secrets.extend(self.backend_recorder.credentials.values())
        digest = fixture.run(["docker", "image", "inspect", IMAGE, "--format", "{{json .RepoDigests}}"], capture=True).strip()
        report = {"status": "failed" if error else "passed", "backend": self.args.backend,
                  "image": IMAGE, "image_digests": json.loads(digest), "passed": len(self.cases),
                  "cases": self.cases, "events": self.events, "error": str(error) if error else None}
        if self.recorders:
            report.update(backend_contracts=self.backend_recorder.requests, callbacks=self.callback_recorder.requests)
        text = json.dumps(report, ensure_ascii=False, indent=2)
        for secret in self.secrets:
            text = text.replace(secret, "[redacted]")
        (self.directory / "report.json").write_text(text, encoding="utf-8")
        if self.server:
            logs = "".join(self.server.lines)
            for secret in self.secrets:
                logs = logs.replace(secret, "[redacted]")
            (self.directory / "metric.log").write_text(logs, encoding="utf-8")
        if self.started_container:
            logs = fixture.run(["docker", "logs", self.container], capture=True, combine=True)
            for secret in self.secrets:
                logs = logs.replace(secret, "[redacted]")
            (self.directory / "symbolicator.log").write_text(logs, encoding="utf-8")
        print(f"Report: {self.directory / 'report.json'}", flush=True)

    def demo(self):
        native = next(event for event in self.events if event["label"] == "NativePdbSourceContext")
        javascript = next(event for event in self.events if event["label"] == "JavaScriptSourceMap")
        text = (f"Metric с настоящей символикацией\n\n{self.http.base}\n"
                f"Логин: {self.email}\nПароль: {self.password}\n"
                f"Проект: Symbolication Demo ({self.project['project_id']})\n\n"
                f"Native: {self.http.base}/events/{native['event_id']}\n"
                f"JavaScript: {self.http.base}/events/{javascript['event_id']}\n\n"
                "В обоих событиях три вызова и исходники вокруг каждой строки; место ошибки выделено. Кнопка переключает исходные кадры.\n"
                f"Для остановки создайте пустой файл STOP в каталоге {self.directory}\n")
        (self.directory / "DEMO-RU.txt").write_text(text, encoding="utf-8")
        print(text, flush=True)

    def cleanup(self):
        if self.server:
            self.server.stop()
        for recorder in self.recorders:
            recorder.close()
        if self.started_container:
            fixture.run(["docker", "stop", "--time", "5", self.container], capture=True, timeout=30)
        if self.created_database:
            if self.args.backend == "postgres":
                fixture.psql("metric_test", f"DROP DATABASE {self.database} WITH (FORCE)")
            else:
                fixture.mongo(self.database, "fixture.dropDatabase()")
        blobs = (self.directory / "blobs").resolve()
        require(blobs.parent == self.directory.resolve() and self.directory.resolve().parent == (ROOT / "target/symbolication-tests").resolve(), "unowned Blob directory")
        if blobs.exists():
            shutil.rmtree(blobs)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--backend", choices=["postgres", "mongodb"], default="postgres")
    parser.add_argument("--binary", default="target/debug/metric-server.exe" if os.name == "nt" else "target/debug/metric-server")
    parser.add_argument("--port", type=int)
    parser.add_argument("--keep", action="store_true", help="keep demo until interrupted; print synthetic login")
    parser.add_argument("--sdk", action="store_true", help="also run both source-map SDK processes and all existing nine SDK fixtures")
    parser.add_argument("--ui", action="store_true", help="verify both resolved event pages and raw-frame toggle in Chromium; requires built web/dist")
    parser.add_argument("--faults", action="store_true", help="also verify HTTP 503 raw fallback and recovery through the complete server")
    args = parser.parse_args()
    suite = Suite(args)
    error = None
    try:
        suite.run()
    except Exception as caught:
        error = caught
    finally:
        try:
            suite.report(error)
        finally:
            if not args.keep or error:
                suite.cleanup()
    if error:
        raise error
    if args.keep:
        suite.demo()
        try:
            while not (suite.directory / "STOP").exists():
                time.sleep(1)
        except KeyboardInterrupt:
            pass
        finally:
            suite.cleanup()
    return 0


if __name__ == "__main__":
    sys.exit(main())
