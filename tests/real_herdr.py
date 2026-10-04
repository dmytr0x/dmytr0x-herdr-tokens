#!/usr/bin/env python3
"""Opt-in, isolated Herdr 0.9.1 acceptance/contract check; never uses a default endpoint."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
from collections.abc import Callable
from typing import Any
from harness_support import cleanup_all, terminate

parser = argparse.ArgumentParser()
parser.add_argument('--herdr', default=shutil.which('herdr'))
parser.add_argument('--binary', default='target/release/herdr-tokens')
parser.add_argument('--capture', action='store_true')
parser.add_argument('--expected-herdr-version', default='0.9.1')
args = parser.parse_args()
ROOT = Path(__file__).resolve().parent.parent
binary = (ROOT / args.binary).resolve()
assert args.herdr and binary.is_file(), 'Build release binary and install Herdr first'


def wait(predicate: Callable[[], object], seconds: float = 10) -> None:
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        if predicate():
            return
        time.sleep(.1)
    raise AssertionError('acceptance deadline exceeded')


with tempfile.TemporaryDirectory(prefix='ht-real-', dir='/tmp') as tmp:
    root = Path(tmp).resolve()
    env = {'PATH': os.environ['PATH'], 'HOME': str(root), 'USER': os.environ.get('USER', 'test'),
           'TERM': 'xterm-256color', 'SHELL': '/bin/sh', 'HERDR_ENV': '1',
           'HERDR_CONFIG_PATH': str(root / 'herdr.toml'), 'HERDR_SOCKET_PATH': str(root / 'api.sock')}
    # Verify that the shipped foreground-style example parses on the real target.
    (root / 'herdr.toml').write_text((ROOT / 'examples/herdr-sidebar.toml').read_text())
    config = root / 'config'
    config.mkdir()
    for number in (1, 2):
        repo = root / f'repo{number}'
        repo.mkdir()
        subprocess.run(['git', 'init', '-q', str(repo)], env=env, check=True)
        for i in range(number):
            (repo / f'file{i}').write_text('untracked')
    def herdr(*cmd: str, check: bool = True) -> subprocess.CompletedProcess[bytes]:
        return subprocess.run([args.herdr, *cmd], env=env, capture_output=True, check=check, timeout=5)
    def tokens(*cmd: str, check: bool = True) -> subprocess.CompletedProcess[bytes]:
        return subprocess.run([str(binary), *cmd, '--config-dir', str(config), '--state-dir', str(root / 'state'),
                               '--runtime-dir', str(root / 'runtime'), '--socket', str(root / 'api.sock'),
                               '--herdr-bin', args.herdr], env=env, capture_output=True, check=check, timeout=7)
    def status() -> dict[str, Any]:
        return json.loads(tokens('status', '--json', '--include-values').stdout)['result']
    def configure(token: str = 'command_value', broken: bool = False) -> None:
        argv = ['/bin/sh', '-c', 'sleep 10' if broken else 'printf \'{"status":"same"}\'']
        text = f'''schema_version=1
[runtime]
discovery_interval_ms=1000
[[collectors]]
name="git"
provider="git"
interval_ms=500
timeout_ms=400
ttl_ms=1500
[collectors.tokens]
git_untracked="untracked_files"
[[collectors]]
name="command"
provider="command"
command={json.dumps(argv)}
interval_ms=500
timeout_ms=200
ttl_ms=1500
[collectors.tokens]
{token}="status"
'''
        path = config / 'tokens.toml'
        path.with_suffix('.tmp').write_text(text)
        path.with_suffix('.tmp').replace(path)
    runners: list[subprocess.Popen] = []
    def start_runner() -> None:
        runner_log = (root / 'runner.log').open('a')
        try:
            runner = subprocess.Popen([str(binary), 'run', '--config-dir', str(config), '--state-dir', str(root / 'state'),
                                       '--runtime-dir', str(root / 'runtime'), '--socket', str(root / 'api.sock'),
                                       '--herdr-bin', args.herdr], env=env, stdout=runner_log, stderr=runner_log)
            runners.append(runner)
        finally:
            runner_log.close()
        wait(lambda: tokens('status', check=False).returncode == 0)
    configure()
    log = (root / 'server.log').open('w+')
    server = subprocess.Popen([args.herdr, 'server'], env=env, stdout=log, stderr=log, start_new_session=True)
    try:
        wait(lambda: (root / 'api.sock').exists())
        assert herdr('--version').stdout.decode().strip() == f'herdr {args.expected_herdr_version}', 'Unexpected Herdr version; explicitly select the version being qualified'
        workspace_ids = []
        for number in (1, 2):
            created = json.loads(herdr('workspace', 'create', '--cwd', str(root / f'repo{number}'), '--no-focus').stdout)
            workspace_ids.append(created['result']['workspace']['workspace_id'])
        workspaces, panes = herdr('workspace', 'list'), herdr('pane', 'list')
        assert len(json.loads(workspaces.stdout)['result']['workspaces']) == 2
        manifest = herdr('plugin', 'link', str(ROOT))
        assert json.loads(manifest.stdout)['result']['plugin']['plugin_id'] == 'dmytr0x-herdr-tokens'
        w = workspace_ids[0]
        good = herdr('workspace', 'report-metadata', w, '--source', 'contract-test', '--seq', '2', '--ttl-ms', '600', '--token', 'contract=first')
        assert good.stdout == b''
        def metadata() -> dict[str, str]:
            info = json.loads(herdr('workspace', 'get', w).stdout)
            return info['result']['workspace'].get('tokens', {})
        assert metadata()['contract'] == 'first'
        herdr('workspace', 'report-metadata', w, '--source', 'contract-test', '--seq', '1', '--ttl-ms', '600', '--token', 'contract=ignored')
        assert metadata()['contract'] == 'first'
        time.sleep(.35)
        herdr('workspace', 'report-metadata', w, '--source', 'contract-test', '--seq', '3', '--ttl-ms', '600', '--token', 'contract=first')
        time.sleep(.35)
        assert metadata()['contract'] == 'first', 'unchanged value must refresh TTL'
        error = herdr('workspace', 'report-metadata', 'missing', '--source', 'contract-test', '--token', 'x=1', check=False)
        assert error.returncode != 0 and json.loads(error.stderr)['error']['code'] == 'workspace_not_found'
        limit = herdr('workspace', 'report-metadata', w, '--source', 'contract-test',
                      *[arg for i in range(17) for arg in ('--token', f'key{i}=x')], check=False)
        assert limit.returncode != 0
        if args.capture:
            fixtures = ROOT / 'tests' / 'fixtures'
            for name, data in [('workspaces-populated.json', workspaces.stdout), ('panes-populated.json', panes.stdout),
                               ('report-success.stdout', good.stdout), ('workspace-not-found.stderr.json', error.stderr)]:
                (fixtures / name).write_bytes(data.replace(str(root).encode(), b'/fixture'))
        tokens('validate')
        start_runner()
        tokens('start')
        tokens('start')
        wait(lambda: len(status()['jobs']) == 4 and all(j['diagnostics']['last_acknowledgement_age_ms'] is not None for j in status()['jobs']))
        values = {j['workspace']: j['diagnostics']['last_acknowledged']['git_untracked'] for j in status()['jobs'] if j['collector'] == 'git'}
        assert sorted(values.values()) == ['1', '2']
        time.sleep(3.3)
        assert metadata()['command_value'] == 'same'
        (config / 'tokens.toml').write_text('schema_version = INVALID')
        assert tokens('reload', check=False).returncode == 2
        assert status()['config_generation'] == 1
        configure('replacement')
        tokens('reload')
        wait(lambda: 'replacement' in metadata() and 'command_value' not in metadata())
        # Script behavior changes without a config reload must expire its last value.
        script = root / 'collector.sh'
        script.write_text('#!/bin/sh\nprintf \'{"status":"good"}\'')
        script.chmod(0o700)
        text = (config / 'tokens.toml').read_text()
        line = next(line for line in text.splitlines() if line.startswith('command='))
        (config / 'tokens.toml').write_text(text.replace(line, 'command=' + json.dumps([str(script)])))
        tokens('reload')
        wait(lambda: metadata().get('replacement') == 'good')
        script.write_text('#!/bin/sh\nsleep 10')
        wait(lambda: 'replacement' not in metadata(), 6)
        assert 'git_untracked' in metadata(), 'hanging collector starved healthy collector'
        script.write_text('#!/bin/sh\nprintf \'{"status":"repaired"}\'')
        wait(lambda: metadata().get('replacement') == 'repaired')
        tokens('stop')
        wait(lambda: tokens('status', check=False).returncode != 0)
        start_runner()
        wait(lambda: metadata().get('replacement') == 'repaired')
        # Keep the emitter alive while restarting only the isolated Herdr server.
        herdr('server', 'stop')
        server.wait(timeout=5)
        wait(lambda: status()['connection'] == 'Disconnected')
        server = subprocess.Popen([args.herdr, 'server'], env=env, stdout=log, stderr=log, start_new_session=True)
        wait(lambda: (root / 'api.sock').exists())
        wait(lambda: status()['connection'] == 'Connected' and metadata().get('replacement') == 'repaired')
        herdr('workspace', 'close', workspace_ids[1])
        wait(lambda: len(status()['jobs']) == 2)
        tokens('stop')
        wait(lambda: 'git_untracked' not in metadata() and 'replacement' not in metadata())
        # Exercise the shipped actions and Herdr's actual injected paths/env.
        wait(lambda: tokens('status', check=False).returncode != 0)
        plugin_config = Path(herdr('plugin', 'config-dir', 'dmytr0x-herdr-tokens').stdout.decode().strip())
        (plugin_config / 'tokens.toml').write_text('schema_version=1\n')
        def action_status() -> subprocess.CompletedProcess[bytes]:
            return subprocess.run([str(binary), 'status', '--json', '--socket', str(root / 'api.sock')],
                                  env=env, capture_output=True, timeout=6)
        herdr('plugin', 'action', 'invoke', 'dmytr0x-herdr-tokens.start')
        wait(lambda: action_status().returncode == 0)
        action_identity = json.loads(action_status().stdout)['identity']
        assert action_identity['config'] == str(plugin_config)
        assert Path(action_identity['state']).is_dir()
        herdr('plugin', 'action', 'invoke', 'dmytr0x-herdr-tokens.reload')
        herdr('plugin', 'action', 'invoke', 'dmytr0x-herdr-tokens.status')
        herdr('plugin', 'action', 'invoke', 'dmytr0x-herdr-tokens.stop')
        wait(lambda: action_status().returncode != 0)
        print(f'PASS: isolated Herdr {args.expected_herdr_version} contract, manifest actions and two-workspace acceptance checks')
    finally:
        cleanup_all(
            lambda: subprocess.run([str(binary), 'stop', '--socket', str(root / 'api.sock')], env=env, capture_output=True, timeout=6),
            lambda: tokens('stop', check=False),
            *(lambda runner=runner: terminate(runner) for runner in runners),
            lambda: herdr('server', 'stop', check=False),
            lambda: terminate(server, group=True),
            log.close,
        )
