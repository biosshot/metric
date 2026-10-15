#!/usr/bin/env python3
"""Run actual installers with a synthetic Docker CLI, without touching real volumes."""
import http.server
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import threading
import unittest

ROOT = Path(__file__).resolve().parents[1]


class Assets(http.server.BaseHTTPRequestHandler):
    requests = 0

    def do_GET(self):
        type(self).requests += 1
        self.send_error(503, 'Synthetic download boundary')

    def log_message(self, *_):
        pass


class VolumeGuard(unittest.TestCase):
    def test_real_installers_distinguish_empty_existing_and_failed_daemon(self):
        shells = []
        for name in ('powershell', 'pwsh'):
            path = shutil.which(name)
            if path:
                shells.append((name, [path, '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', str(ROOT / 'deploy/install.ps1')]))
        shell = shutil.which('sh')
        if shell:
            shells.append(('sh', [shell, str(ROOT / 'deploy/install.sh')]))
        self.assertTrue(shells, 'No supported installer shell found')
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Assets)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            for name, command in shells:
                for scenario in ('empty', 'legacy', 'project', 'daemon'):
                    with self.subTest(shell=name, scenario=scenario), tempfile.TemporaryDirectory(prefix='metric-volume-guard-') as scratch:
                        folder = Path(scratch)
                        stub = folder / ('docker.cmd' if os.name == 'nt' and name != 'sh' else 'docker')
                        if stub.suffix == '.cmd':
                            stub.write_text('@echo off\nif "%1"=="compose" exit /b 0\nif not "%2"=="ls" exit /b 88\n'
                                + ('echo Synthetic daemon failure 1>&2\nexit /b 1\n' if scenario == 'daemon' else
                                   ('echo metric_mongo-data\n' if scenario == 'legacy' else 'echo guard_postgres-data\n' if scenario == 'project' else '') + 'exit /b 0\n'))
                        else:
                            stub.write_text('#!/bin/sh\ncat >/dev/null\n[ "$1" = compose ] && exit 0\n[ "$2" = ls ] || exit 88\n'
                                + ('echo "Synthetic daemon failure" >&2\nexit 1\n' if scenario == 'daemon' else
                                   ('echo metric_mongo-data\n' if scenario == 'legacy' else 'echo guard_postgres-data\n' if scenario == 'project' else '') + 'exit 0\n'))
                            stub.chmod(0o755)
                        env = {key: value for key, value in os.environ.items() if not key.startswith(('METRIC_', 'APP__', 'COMPOSE_')) and key != 'MONGODB_URI'}
                        # A PowerShell 7 parent can export module paths incompatible with 5.1.
                        for key in list(env):
                            if key.lower() == 'psmodulepath':
                                env.pop(key)
                        env.update(PATH=str(folder) + os.pathsep + env['PATH'],
                            METRIC_INSTALL_DIR=str(folder / 'install'), METRIC_PROFILE='min',
                            COMPOSE_PROJECT_NAME='guard',
                            METRIC_DOWNLOAD_BASE=f'http://127.0.0.1:{server.server_port}')
                        before = Assets.requests
                        # A Docker CLI that reads stdin must not eat a piped installer.
                        invocation = [command[0]] if name == 'sh' else command
                        input_args = {'input': (ROOT / 'deploy/install.sh').read_bytes()} if name == 'sh' else {'stdin': subprocess.DEVNULL}
                        result = subprocess.run(invocation, env=env, cwd=folder,
                            capture_output=True, timeout=30, **input_args)
                        self.assertNotEqual(result.returncode, 0)
                        self.assertEqual(Assets.requests > before, scenario == 'empty', result.stdout + result.stderr)
                        self.assertFalse((folder / 'install/.env').exists())
                        if scenario in ('legacy', 'project'):
                            self.assertIn(b'Restore the original', result.stdout + result.stderr)
        finally:
            server.shutdown()
            server.server_close()
            thread.join()


if __name__ == '__main__':
    unittest.main()
