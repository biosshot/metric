#!/usr/bin/env python3
"""Exercise actual shell/PowerShell installers against isolated real Docker databases."""
from __future__ import annotations

import argparse
import hashlib
import http.server
import importlib.util
import json
import os
from pathlib import Path
import re
import socket
import shutil
import subprocess
import sys
import threading
import time
import urllib.parse
import uuid

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location('installer_http', ROOT / 'scripts/test-postgres.py')
HTTP = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(HTTP)
RUN = uuid.uuid4().hex
LABEL = 'metric.installer-test=' + RUN
PASSWORD = "installer'quote$literal-only"


def run(args, *, env=None, cwd=ROOT, timeout=180, expected=0):
    if Path(str(args[0])).name.lower() in ('powershell', 'powershell.exe'):
        env = (env or os.environ).copy()
        for key in list(env):
            if key.lower() == 'psmodulepath':
                env.pop(key)
    result = subprocess.run(args, env=env, cwd=cwd, stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, text=True, encoding='utf-8', errors='replace', timeout=timeout)
    expected_codes = (expected,) if isinstance(expected, int) else expected
    if result.returncode not in expected_codes:
        # Installer output can contain the visibly entered URI in an interactive test.
        output = re.sub(r'METRIC_BOOTSTRAP_TOKEN=\S+', 'METRIC_BOOTSTRAP_TOKEN=<redacted>', result.stdout)
        output = output.replace(PASSWORD, '<fixture-secret>').replace(urllib.parse.quote(PASSWORD, safe=''), '<fixture-secret>')
        raise RuntimeError(f'{Path(str(args[0])).name} returned {result.returncode}, expected {expected}:\n{output[-6000:]}')
    return result.stdout


def port():
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        return listener.getsockname()[1]


def wizard_run(command, env, answers):
    """Drive real terminal prompts, including pipelines and visibly entered URI."""
    output = []
    if os.name == 'nt':
        from winpty import PtyProcess
        process = PtyProcess.spawn(command, cwd=str(ROOT), env=env, dimensions=(40, 180))
        read = lambda: process.read(4096)
        # A terminal Enter is CR. Sending CRLF injects two answers into a Linux
        # terminal reached through WSL/ConPTY.
        write = lambda value: process.write(value + '\r')
        alive = process.isalive
        finish = process.wait
        cancel = lambda: process.terminate(force=True)
    else:
        import fcntl
        import pty
        import signal
        import struct
        import termios
        pid, descriptor = pty.fork()
        if pid == 0:
            # Match the Windows PTY: a zero-sized terminal breaks Read-Host.
            fcntl.ioctl(1, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 180, 0, 0))
            os.chdir(ROOT)
            os.execvpe(command[0], command, env)
        state = {'exit': None}
        def alive():
            if state['exit'] is None:
                child, status = os.waitpid(pid, os.WNOHANG)
                if child:
                    state['exit'] = os.waitstatus_to_exitcode(status)
            return state['exit'] is None
        read = lambda: os.read(descriptor, 4096).decode('utf-8', errors='replace')
        write = lambda value: os.write(descriptor, (value + '\n').encode())
        finish = lambda: state['exit']
        cancel = lambda: os.kill(pid, signal.SIGTERM)
    def reader():
        query_tail = ''
        try:
            while True:
                chunk = read()
                if not chunk:
                    return
                output.append(chunk)
                if os.name != 'nt':
                    # PowerShell's console reader queries its cursor position.
                    # A raw PTY has no terminal emulator to answer that request.
                    queries = query_tail + chunk
                    for _ in range(queries.count('\x1b[6n')):
                        os.write(descriptor, b'\x1b[1;1R')
                    query_tail = queries[-3:]
        except (EOFError, OSError):
            return
    threading.Thread(target=reader, daemon=True).start()
    deadline = time.monotonic() + 240
    try:
        for label, value in answers:
            while label not in ''.join(output):
                if not alive() or time.monotonic() > deadline:
                    trace = ''.join(output).replace(PASSWORD, '<fixture-secret>').replace(urllib.parse.quote(PASSWORD, safe=''), '<fixture-secret>')
                    raise RuntimeError('Wizard did not reach expected prompt: ' + label + '\n' + trace[-2500:])
                time.sleep(0.1)
            try:
                write(value)
            except EOFError:
                trace = ''.join(output).replace(PASSWORD, '<fixture-secret>').replace(urllib.parse.quote(PASSWORD, safe=''), '<fixture-secret>')
                raise RuntimeError('Wizard closed before answering ' + label + '\n' + trace[-3000:]) from None
        while alive():
            if time.monotonic() > deadline:
                raise RuntimeError('Wizard terminal deadline exceeded')
            time.sleep(0.2)
        if finish() != 0:
            text = ''.join(output).replace(PASSWORD, '<fixture-secret>').replace(urllib.parse.quote(PASSWORD, safe=''), '<fixture-secret>')
            raise RuntimeError('Wizard failed: ' + text[-3000:])
        return ''.join(output)
    finally:
        if alive():
            cancel()


class Assets(http.server.BaseHTTPRequestHandler):
    files = {}
    network = ''
    def log_message(self, *args):
        pass

    def do_GET(self):
        name = self.path.lstrip('/')
        if name not in self.files:
            self.send_error(404)
            return
        data = self.files[name]
        # Real local release image; no registry publication is performed by this gate.
        # Existing database/Symbolicator images are also reused. All other assets
        # and the downloaded installers are unchanged.
        if name.endswith('.yml') and name.startswith('compose'):
            data = re.sub(r'(?m)^(    image: .*\n)', r'\1    pull_policy: never\n', data)
            if self.network:
                data = data.replace('  metric:\n', '  metric:\n    networks: [default, installer-database]\n')
                data += '\nnetworks:\n  installer-database:\n    external: true\n    name: ' + self.network + '\n'
        body = data.encode('utf-8')
        self.send_response(200)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def own_container(name):
    actual = run(['docker', 'inspect', name, '--format', '{{index .Config.Labels "metric.installer-test"}}']).strip()
    if actual != RUN:
        raise RuntimeError('Refusing cleanup of a container not owned by this fixture')


def compose(folder, project, *args):
    return run(['docker', 'compose', '-p', project, '-f', str(folder / 'compose.yml'), *args], timeout=180)


def hashes(folder):
    return {name: hashlib.sha256((folder / name).read_bytes()).hexdigest()
            for name in ('compose.yml', 'metric.toml', '.env', 'connection.env') if (folder / name).exists()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--image', default='metric-postgres:local')
    parser.add_argument('--shell', default='sh')
    parser.add_argument('--powershell', default='pwsh')
    parser.add_argument('--only', choices=('shell', 'powershell'))
    parser.add_argument('--case', choices=('legacy', 'bundled-postgres', 'external-postgres', 'external-mongodb'))
    parser.add_argument('--transport', choices=('tcp', 'tls'), default='tcp')
    parser.add_argument('--wizard', action='store_true')
    parser.add_argument('--profiles-only', action='store_true')
    parser.add_argument('--negative-only', action='store_true')
    parser.add_argument('--wsl', help='Windows fixture: exercise native shell in this WSL distro without changing Docker integration')
    args = parser.parse_args()
    if args.negative_only:
        scratch = ROOT / 'target' / ('installer-negative-' + RUN)
        scratch.mkdir(parents=True)
        for engine, executable in [('shell', args.shell), ('powershell', args.powershell)]:
            env = os.environ.copy()
            for name in list(env):
                if name.startswith(('METRIC_', 'APP__')) or name == 'MONGODB_URI':
                    env.pop(name)
            env['METRIC_INSTALL_DIR'] = str(scratch / engine)
            if os.name == 'nt':
                env['PATH'] = r'C:\Program Files\Git\usr\bin;' + env['PATH']
            prefix = [executable, str(ROOT / 'deploy/install.sh')] if engine == 'shell' else [executable, '-NoProfile', '-File', str(ROOT / 'deploy/install.ps1')]
            option = lambda name: '--' + name if engine == 'shell' else '-' + {'http-port': 'HttpPort', 'backend': 'Backend', 'database': 'Database', 'profile': 'Profile', 'interactive': 'Interactive', 'tls': 'Tls', 'help': 'Help'}[name]
            help_output = run([*prefix, option('help')], env=env)
            if 'METRIC_DATABASE_BACKEND' not in help_output or 'METRIC_HTTP_PORT' not in help_output:
                raise RuntimeError('Installer help omits public options')
            for flags in ([option('profile'), 'invalid'], [option('http-port'), '65536'],
                          [option('http-port'), 'abc'], [option('backend'), 'invalid'],
                          [option('backend'), 'postgres', option('database'), 'external']):
                run([*prefix, *flags], env=env, expected=1, timeout=20)
            # POSIX shells may exit 2 on the exec redirection to an unavailable
            # /dev/tty; Bash and PowerShell use 1. Both must refuse installation.
            run([*prefix, option('interactive')], env=env,
                expected=(1, 2) if engine == 'shell' else 1, timeout=20)
            env['METRIC_POSTGRES_URI'] = 'postgres://metric:fixture@db.example:5432/metric?sslmode=disable'
            run([*prefix, option('backend'), 'postgres', option('database'), 'external', option('tls'), 'on'], env=env, expected=1)
            if (scratch / engine / '.env').exists():
                raise RuntimeError('Invalid request generated installation secrets')
            print(f'Passed invalid arguments, missing URI, no-terminal wizard, TLS conflict and help: {engine}', flush=True)
        return
    run(['docker', 'image', 'inspect', args.image])
    if args.profiles_only:
        for backend in ('mongodb', 'postgres'):
            for profile in ('min', 'low', 'medium', 'high'):
                relative = f'profiles/{profile}.toml' if backend == 'mongodb' else f'profiles/postgres/{profile}.toml'
                source = (ROOT / 'deploy' / relative).as_posix()
                output = run(['docker', 'run', '--rm', '--label', LABEL,
                              '-v', source + ':/etc/metric/config.toml:ro',
                              '-e', 'MONGODB_URI=mongodb://metric:fixture@db.example:27017/?authSource=admin',
                              '-e', 'METRIC_POSTGRES_URI=postgres://metric:fixture@db.example:5432/metric',
                              '-e', 'SCRUB_HMAC_KEY=' + '09' * 32,
                              args.image, '--config', '/etc/metric/config.toml', '--check-config'])
                if 'configuration is valid' not in output:
                    raise RuntimeError('Actual config validation did not pass')
                print(f'Profile passed: {backend}/{profile}', flush=True)
        return
    folder_root = ROOT / 'target' / ('installer-' + RUN)
    folder_root.mkdir(parents=True)
    # Freeze sources for a whole run; editing an executing shell file can tear its
    # buffered input and invalidate the result. No live workspace code is executed.
    for pattern in ('compose*.yml', 'install.sh', 'install.ps1', 'symbolicator.yml', 'profiles/*.toml', 'profiles/*.env.example', 'profiles/postgres/*.toml'):
        for source in (ROOT / 'deploy').glob(pattern):
            Assets.files[source.relative_to(ROOT / 'deploy').as_posix()] = source.read_text(encoding='utf-8')
    for name in ('install.sh', 'install.ps1'):
        shutil.copyfile(ROOT / 'deploy' / name, folder_root / name)
    def linux_path(path):
        value = str(path).replace('\\', '/')
        return '/mnt/' + value[0].lower() + value[2:]
    shell_prefix = [args.shell]
    if args.wsl:
        bridge = folder_root / 'wsl-bin'
        bridge.mkdir()
        (bridge / 'docker').write_text('#!/bin/sh\nexec "/mnt/c/Program Files/Docker/Docker/resources/bin/docker.exe" "$@"\n', encoding='utf-8', newline='\n')
        (bridge / 'curl').write_text('''#!/bin/sh
if [ "$#" -ge 2 ]; then
  case "$1" in --fail) exec "/mnt/c/Windows/System32/curl.exe" --fail --location --silent --show-error "$5" --output "$(wslpath -w "$7")" ;; esac
fi
exec "/mnt/c/Windows/System32/curl.exe" "$@"
''', encoding='utf-8', newline='\n')
        shell_prefix = ['wsl.exe', '-d', args.wsl, '--', 'env',
                        'PATH=' + linux_path(bridge) + ':/usr/local/bin:/usr/bin:/bin', 'sh']
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Assets)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    base = f'http://127.0.0.1:{server.server_port}'
    pg_name, mongo_name = 'metric-installer-pg-' + RUN, 'metric-installer-mongo-' + RUN
    pg_port, mongo_port = port(), port()
    fixtures = []
    network = ''
    certificate_volume = ''
    results = []
    try:
        pg_extra, mongo_extra = [], []
        if args.transport == 'tls':
            network = 'metric-installer-net-' + RUN
            certificate_volume = 'metric-installer-certs-' + RUN
            run(['docker', 'network', 'create', '--label', LABEL, network])
            run(['docker', 'volume', 'create', '--label', LABEL, certificate_volume])
            Assets.network = network
            certificates = folder_root / 'certificates'
            certificates.mkdir()
            generator_script = (ROOT / 'deploy/postgres-test-certs.sh').read_text(encoding='utf-8').replace(
                'DNS:postgres-tls,IP:127.0.0.1', 'DNS:postgres-tls,DNS:mongodb-tls,DNS:host.docker.internal,IP:127.0.0.1')
            generator_script += '\ncat server.crt server.key > server.pem\ncat client.crt client.key > client.pem\nchmod 600 server.pem client.pem\nchown 999:999 server.pem\n'
            script_file = certificates / 'generate.sh'
            script_file.write_text(generator_script, encoding='utf-8', newline='\n')
            generator = 'metric-installer-generator-' + RUN
            run(['docker', 'run', '--name', generator, '--label', LABEL, '--user', 'root', '--entrypoint', 'sh',
                 '-v', f'{script_file.as_posix()}:/generate.sh:ro', '-v', f'{certificate_volume}:/certs',
                 'postgres:18.6-bookworm', '/generate.sh'])
            fixtures.append(generator)
            for name in ('ca.crt', 'wrong-ca.crt', 'client.crt', 'client.key'):
                run(['docker', 'cp', f'{generator}:/certs/{name}', str(certificates / name)])
            pg_extra = ['--network', network, '--network-alias', 'postgres-tls', '--network-alias', 'wrong-pg', '-v', f'{certificate_volume}:/run/installer-certs:ro']
            mongo_extra = ['--network', network, '--network-alias', 'mongodb-tls', '--network-alias', 'wrong-mongo', '-v', f'{certificate_volume}:/run/installer-certs:ro']
        run(['docker', 'run', '-d', '--name', pg_name, '--label', LABEL,
             '-p', f'{pg_port}:5432', '-e', 'POSTGRES_USER=metric', '-e', 'POSTGRES_DB=metric',
             '-e', 'POSTGRES_PASSWORD=' + PASSWORD, *pg_extra, 'postgres:18.6-bookworm',
             *(['postgres', '-c', 'ssl=on', '-c', 'ssl_cert_file=/run/installer-certs/server.crt',
                '-c', 'ssl_key_file=/run/installer-certs/server.key', '-c', 'ssl_ca_file=/run/installer-certs/ca.crt'] if network else [])])
        fixtures.append(pg_name)
        run(['docker', 'run', '-d', '--name', mongo_name, '--label', LABEL,
             '-p', f'{mongo_port}:27017', '-e', 'MONGO_INITDB_ROOT_USERNAME=metric',
             '-e', 'MONGO_INITDB_ROOT_PASSWORD=' + PASSWORD, *mongo_extra, 'mongo:8.0.12',
             *(['mongod', '--tlsMode', 'requireTLS', '--tlsCAFile', '/run/installer-certs/ca.crt',
                '--tlsCertificateKeyFile', '/run/installer-certs/server.pem'] if network else [])])
        fixtures.append(mongo_name)
        for name, command in [(pg_name, ['pg_isready', '-U', 'metric', '-d', 'metric']),
                              (mongo_name, ['mongosh', '--quiet',
                                            *(['--tls', '--host', 'mongodb-tls', '--tlsCAFile', '/run/installer-certs/ca.crt',
                                               '--tlsCertificateKeyFile', '/run/installer-certs/client.pem'] if network else []),
                                            '--eval', 'db.adminCommand({ping:1}).ok'])]:
            deadline = time.monotonic() + 60
            while time.monotonic() < deadline:
                probe = subprocess.run(['docker', 'exec', name, *command], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                if probe.returncode == 0:
                    break
                time.sleep(1)
            else:
                raise RuntimeError('Owned external database fixture did not start')
        if network:
            # Require a verified client certificate in addition to password auth.
            # Change only this run's owned PostgreSQL fixture, never a user server.
            own_container(pg_name)
            hba = 'local all all trust\nhostssl all all all scram-sha-256 clientcert=verify-ca\nhostnossl all all all reject\n'
            result = subprocess.run(['docker', 'exec', '-i', pg_name, 'sh', '-c', 'cat > "$PGDATA/pg_hba.conf"'],
                                    input=hba, text=True, capture_output=True)
            if result.returncode:
                raise RuntimeError('Cannot configure owned mutual TLS fixture')
            run(['docker', 'exec', pg_name, 'psql', '-U', 'metric', '-d', 'metric', '-c', 'SELECT pg_reload_conf()'])
        engines = ['shell', 'powershell'] if not args.only else [args.only]
        cases = (['external-postgres', 'external-mongodb'] if network else ['legacy', 'bundled-postgres', 'external-postgres', 'external-mongodb']) if not args.case else [args.case]
        for engine in engines:
            for case in cases:
                project = 'metric-installer-' + uuid.uuid4().hex
                folder = folder_root / (engine + '-' + case)
                folder.mkdir()
                env = os.environ.copy()
                for name in list(env):
                    if name.startswith(('METRIC_', 'APP__')) or name in ('MONGODB_URI', 'SCRUB_HMAC_KEY', 'PGSSLROOTCERT', 'PGSSLCERT', 'PGSSLKEY', 'PGOPTIONS'):
                        env.pop(name)
                env.update({'METRIC_DOWNLOAD_BASE': base, 'METRIC_INSTALL_DIR': str(folder),
                            'METRIC_IMAGE': args.image, 'METRIC_HTTP_PORT': str(port()),
                            'COMPOSE_PROJECT_NAME': project, 'MSYS_NO_PATHCONV': '1'})
                expected_port = int(env['METRIC_HTTP_PORT'])
                if os.name == 'nt':
                    env['PATH'] = r'C:\Program Files\Git\usr\bin;' + env['PATH']
                backend = 'postgres' if case.endswith('postgres') else 'mongodb'
                mode = 'external' if case.startswith('external') else 'bundled'
                flags = []
                if case != 'legacy':
                    env['METRIC_PROFILE'] = 'min'
                    flags = (['--backend', backend, '--database', mode] if engine == 'shell'
                             else ['-Backend', backend, '-Database', mode])
                if case == 'bundled-postgres':
                    env.update({'METRIC_HTTP_PORT': '0', 'METRIC_PROFILE': 'high'})
                    flags += (['--http-port', str(expected_port), '--profile', 'min'] if engine == 'shell'
                              else ['-HttpPort', str(expected_port), '-Profile', 'min'])
                if mode == 'external':
                    database = 'inst_' + uuid.uuid4().hex
                    if backend == 'postgres':
                        run(['docker', 'exec', pg_name, 'psql', '-U', 'metric', '-d', 'metric', '-c', 'CREATE DATABASE ' + database])
                        endpoint = 'postgres-tls:5432' if network else f'localhost:{pg_port}'
                        ssl = 'verify-full' if network else 'disable'
                        env['METRIC_POSTGRES_URI'] = f'postgres://metric:{urllib.parse.quote(PASSWORD, safe="")}@{endpoint}/{database}?sslmode={ssl}'
                    else:
                        endpoint = 'mongodb-tls:27017' if network else f'localhost:{mongo_port}'
                        tls = 'true' if network else 'false'
                        env['MONGODB_URI'] = f'mongodb://metric:{urllib.parse.quote(PASSWORD, safe="")}@{endpoint}/{database}?authSource=admin&tls={tls}'
                    if network:
                        env.update({'METRIC_DATABASE_TLS_CA': str(certificates / 'ca.crt'),
                                    'METRIC_DATABASE_TLS_CLIENT_CERT': str(certificates / 'client.crt'),
                                    'METRIC_DATABASE_TLS_CLIENT_KEY': str(certificates / 'client.key')})
                # Old download pipelines and new parameterized downloaded scripts.
                if args.wsl and engine == 'shell':
                    names = [name + ('/p' if name in ('METRIC_INSTALL_DIR', 'METRIC_DATABASE_TLS_CA', 'METRIC_DATABASE_TLS_CLIENT_CERT', 'METRIC_DATABASE_TLS_CLIENT_KEY') else '')
                             for name in env if name.startswith('METRIC_') or name in ('MONGODB_URI', 'COMPOSE_PROJECT_NAME')]
                    env['WSLENV'] = ':'.join(names + ['METRIC_INTERACTIVE', 'METRIC_DATABASE_BACKEND', 'METRIC_DATABASE_MODE', 'METRIC_POSTGRES_URI', 'MONGODB_URI'])
                if engine == 'shell':
                    command = [*shell_prefix, '-c', 'curl -fsSL "$METRIC_DOWNLOAD_BASE/install.sh" | sh'] if case == 'legacy' else [*shell_prefix, linux_path(folder_root / 'install.sh') if args.wsl else str(folder_root / 'install.sh'), *flags]
                else:
                    command = [args.powershell, '-NoProfile', '-Command', 'irm "$env:METRIC_DOWNLOAD_BASE/install.ps1" | iex'] if case == 'legacy' else [args.powershell, '-NoProfile', '-File', str(folder_root / 'install.ps1'), *flags]
                try:
                    print(f'Running {engine}: {case}', flush=True)
                    run_installer = lambda: run(command, env=env, timeout=240)
                    if args.wizard:
                        env['METRIC_INTERACTIVE'] = '1'
                        supplied_uri = env.pop('METRIC_POSTGRES_URI', '') if backend == 'postgres' else env.pop('MONGODB_URI', '')
                        answers = [('Database (mongodb/postgres)', backend), ('Database placement', mode)]
                        if mode == 'external':
                            answers.append(('Connection URI', supplied_uri))
                        answers += [('Resource profile', 'medium' if case == 'legacy' else 'min'), ('Metric HTTP port', str(expected_port))]
                        run_installer = lambda: wizard_run(command, env, answers)
                    output = run_installer()
                    if not args.wizard and (PASSWORD in output or urllib.parse.quote(PASSWORD, safe='') in output):
                        raise RuntimeError('Installer diagnostic leaked connection credentials')
                    config = json.loads(compose(folder, project, 'config', '--format', 'json'))
                    expected_services = {'metric'} | ({'mongodb' if backend == 'mongodb' else 'postgres'} if mode == 'bundled' else set())
                    if case == 'legacy':
                        expected_services |= {'symbolicator', 'symbolicator-cleanup'}
                    if set(config['services']) != expected_services:
                        raise RuntimeError('Selected services do not match backend/placement/profile')
                    if mode == 'bundled' and config['services'][backend if backend == 'postgres' else 'mongodb'].get('ports'):
                        raise RuntimeError('Bundled database unexpectedly publishes a host port')
                    http_port = expected_port
                    client = HTTP.Http(f'http://127.0.0.1:{http_port}')
                    container = compose(folder, project, 'ps', '-q', 'metric').strip()
                    deadline = time.monotonic() + 30
                    while not client.ready() and time.monotonic() < deadline:
                        time.sleep(0.5)
                    if not client.ready():
                        logs = run(['docker', 'logs', container])
                        logs = re.sub(r'METRIC_BOOTSTRAP_TOKEN=\S+', '<setup-token-redacted>', logs)
                        raise RuntimeError('Installed app is not ready: ' + logs[-2500:])
                    logs = run(['docker', 'logs', container])
                    token = re.search(r'METRIC_BOOTSTRAP_TOKEN=(\S+)', logs)
                    if not token:
                        raise RuntimeError('Fresh owned installation did not emit setup token')
                    # Real account/project, acknowledged ingest, scoped CRUD and reads.
                    HTTP.exercise_http(client, token[1], backend=backend)
                    before = hashes(folder)
                    if args.wizard:
                        # An existing installation must not unexpectedly rerun the wizard.
                        run(command, env=env, timeout=240)
                    else:
                        run_installer()
                    if hashes(folder) != before:
                        raise RuntimeError('Installer rerun changed configuration or permanent secrets')
                    conflicting = env.copy()
                    conflicting['METRIC_DATABASE_BACKEND'] = 'postgres' if backend == 'mongodb' else 'mongodb'
                    conflict_command = command.copy()
                    backend_flag = '--backend' if engine == 'shell' else '-Backend'
                    if backend_flag in conflict_command:
                        conflict_command[conflict_command.index(backend_flag) + 1] = conflicting['METRIC_DATABASE_BACKEND']
                    run(conflict_command, env=conflicting, expected=1, timeout=30)
                    if hashes(folder) != before:
                        raise RuntimeError('Backend conflict changed existing configuration')
                    secret_file, hidden = folder / '.env', folder / '.env.fixture-hidden'
                    secret_file.rename(hidden)
                    try:
                        missing_env = env.copy()
                        missing_env['METRIC_INTERACTIVE'] = '0'
                        if args.wizard and mode == 'external':
                            missing_env['METRIC_POSTGRES_URI' if backend == 'postgres' else 'MONGODB_URI'] = supplied_uri
                        guarded = run(command, env=missing_env, expected=1, timeout=30)
                        if '.env is missing' not in guarded:
                            raise RuntimeError('Missing-secret case failed for a reason other than the data guard')
                    finally:
                        hidden.rename(secret_file)
                    if hashes(folder) != before:
                        raise RuntimeError('Missing-secret guard changed populated installation')
                    if network:
                        # Exercise the generated configuration itself. One-off app
                        # processes do not restart and must fail closed; the user's
                        # database and developer installation are never used.
                        ca_path = folder / 'certs/ca.pem'
                        good_ca = ca_path.read_bytes()
                        try:
                            ca_path.write_bytes((certificates / 'wrong-ca.crt').read_bytes())
                            run(['docker', 'compose', '-p', project, '-f', str(folder / 'compose.yml'),
                                 'run', '--rm', '--no-deps', 'metric', '--config', '/etc/metric/metric.toml'],
                                expected=1, timeout=45)
                        finally:
                            ca_path.write_bytes(good_ca)
                        client_path = folder / ('certs/client.key' if backend == 'postgres' else 'certs/client.pem')
                        good_client = client_path.read_bytes()
                        try:
                            client_path.write_bytes(b'not a TLS key\n')
                            run(['docker', 'compose', '-p', project, '-f', str(folder / 'compose.yml'),
                                 'run', '--rm', '--no-deps', 'metric', '--config', '/etc/metric/metric.toml'],
                                expected=1, timeout=45)
                        finally:
                            client_path.write_bytes(good_client)
                        good_environment = secret_file.read_bytes()
                        try:
                            old_host, wrong_host = ('postgres-tls', 'wrong-pg') if backend == 'postgres' else ('mongodb-tls', 'wrong-mongo')
                            secret_file.write_bytes(good_environment.replace(old_host.encode(), wrong_host.encode()))
                            run(['docker', 'compose', '-p', project, '-f', str(folder / 'compose.yml'),
                                 'run', '--rm', '--no-deps', 'metric', '--config', '/etc/metric/metric.toml'],
                                expected=1, timeout=45)
                        finally:
                            secret_file.write_bytes(good_environment)
                        if hashes(folder) != before:
                            raise RuntimeError('Protected failure tests did not restore installed settings')
                    compose(folder, project, 'restart', 'metric')
                    deadline = time.monotonic() + 30
                    while not client.ready() and time.monotonic() < deadline:
                        time.sleep(0.5)
                    if not client.ready():
                        raise RuntimeError('Installed app did not recover after restart')
                    results.append({'engine': engine, 'case': case, 'wizard': args.wizard, 'passed': True})
                    (folder_root / 'results.json').write_text(json.dumps({'run': RUN, 'transport': args.transport, 'results': results}, indent=2) + '\n', encoding='utf-8')
                    print(f'Passed {engine}: {case}', flush=True)
                finally:
                    if (folder / 'compose.yml').exists():
                        # Explicit UUID project generated here; never clean the developer's projects.
                        for container_id in compose(folder, project, 'ps', '-a', '-q').splitlines():
                            label = run(['docker', 'inspect', container_id, '--format', '{{index .Config.Labels "com.docker.compose.project"}}']).strip()
                            if label != project:
                                raise RuntimeError('Refusing cleanup of a foreign Compose container')
                        compose(folder, project, 'down', '--volumes', '--remove-orphans')
        report = folder_root / 'results.json'
        report.write_text(json.dumps({'run': RUN, 'results': results}, indent=2) + '\n', encoding='utf-8')
        print(f'Installer functional gates passed: {len(results)}; report {report}', flush=True)
    finally:
        server.shutdown(); server.server_close()
        for name in fixtures:
            own_container(name)
            run(['docker', 'rm', '-f', '-v', name])
        for kind, name in [('network', network), ('volume', certificate_volume)]:
            if name:
                actual = run(['docker', kind, 'inspect', name, '--format', '{{index .Labels "metric.installer-test"}}']).strip()
                if actual != RUN:
                    raise RuntimeError('Refusing cleanup of foreign fixture storage/network')
                run(['docker', kind, 'rm', name])


if __name__ == '__main__':
    main()
