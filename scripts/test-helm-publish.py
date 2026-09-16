#!/usr/bin/env python3
"""Check immutable publication and anonymous access gates without a remote write."""

from __future__ import annotations

import importlib.util
import io
import json
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location("publish_helm", Path(__file__).with_name("publish-helm-chart.py"))
PUBLISH = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PUBLISH)


def package(path: Path, content: bytes = b"version: 0.1.7\n", timestamp: int = 0) -> None:
    with tarfile.open(path, "w:gz") as archive:
        entry = tarfile.TarInfo("metric/Chart.yaml")
        entry.size = len(content)
        entry.mtime = timestamp
        archive.addfile(entry, io.BytesIO(content))


class PublicationTests(unittest.TestCase):
    def exercise(self, prior: str = "missing", anonymous: str = "public") -> list:
        calls = []
        with tempfile.TemporaryDirectory(prefix="metric-publish-test-") as temp:
            folder = Path(temp)
            archive = folder / "metric-0.1.7.tgz"
            package(archive)

            def registry(command, **kwargs):
                calls.append(command)
                if command[1] == "push":
                    return subprocess.CompletedProcess(command, 0, "Pushed\n", "")
                destination = Path(command[command.index("--destination") + 1])
                if destination.name == "prior":
                    if prior in ("same", "different"):
                        package(destination / archive.name, b"changed" if prior == "different" else b"version: 0.1.7\n", timestamp=99)
                        return subprocess.CompletedProcess(command, 0, "", "")
                    error = {
                        "missing": "Error: registry.invalid/charts/metric:0.1.7: not found",
                        "denied": "403 denied",
                        "network": "connection refused",
                        "broken-blob": "GET registry.invalid/v2/charts/metric/blobs/sha256:abc: 404 not found",
                    }[prior]
                    return subprocess.CompletedProcess(command, 1, "", error)
                env = kwargs["env"]
                self.assertEqual(Path(env["DOCKER_CONFIG"]), destination)
                expected_config = {"auths": {"registry.invalid": {}}}
                self.assertEqual(json.loads(Path(env["HELM_REGISTRY_CONFIG"]).read_text()), expected_config)
                self.assertEqual(json.loads((destination / "config.json").read_text()), expected_config)
                if anonymous == "private":
                    return subprocess.CompletedProcess(command, 1, "", "403 denied")
                if anonymous == "different":
                    package(destination / archive.name, b"changed")
                else:
                    shutil.copyfile(archive, destination / archive.name)
                return subprocess.CompletedProcess(command, 0, "", "")

            self.calls = calls
            with patch.object(PUBLISH.subprocess, "run", side_effect=registry):
                PUBLISH.publish(archive, "oci://registry.invalid/charts", "0.1.7", folder)
        return calls

    def test_first_publish(self):
        self.assertEqual([c[1] for c in self.exercise()], ["pull", "push", "pull"])

    def test_identical_retry_does_not_push(self):
        self.assertEqual([c[1] for c in self.exercise(prior="same")], ["pull", "pull"])

    def test_existing_different_content_is_not_overwritten(self):
        with self.assertRaisesRegex(RuntimeError, "different content"):
            self.exercise(prior="different")
        self.assertEqual(len(self.calls), 1)

    def test_permission_error_does_not_push(self):
        with self.assertRaisesRegex(RuntimeError, "Could not check"):
            self.exercise(prior="denied")
        self.assertEqual(len(self.calls), 1)

    def test_network_error_does_not_push(self):
        with self.assertRaisesRegex(RuntimeError, "Could not check"):
            self.exercise(prior="network")
        self.assertEqual(len(self.calls), 1)

    def test_private_package_is_not_success(self):
        with self.assertRaisesRegex(RuntimeError, "anonymous pull failed"):
            self.exercise(anonymous="private")

    def test_missing_blob_is_not_permission_to_overwrite_a_manifest(self):
        with self.assertRaisesRegex(RuntimeError, "Could not check"):
            self.exercise(prior="broken-blob")
        self.assertEqual(len(self.calls), 1)

    def test_anonymous_content_must_match(self):
        with self.assertRaisesRegex(RuntimeError, "differs from"):
            self.exercise(anonymous="different")


if __name__ == "__main__":
    unittest.main()
