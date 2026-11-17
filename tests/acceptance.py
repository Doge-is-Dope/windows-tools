"""Acceptance against a running host on Windows: commands, the lease and who is let in.

    python tests/acceptance.py --url https://<IP>:8767 --token-file tools.token --fingerprint <SHA-256>

The host must be idle and human-owned. Assertions need Python without -O. Nothing runs on import.
"""
from __future__ import annotations

import argparse
import json
import threading
import time
from pathlib import Path

from client import Client, HostError, Lease, expect_error


def check_run(client: Client) -> list[str]:
    """exec.run: one PowerShell command, waited for. `client` holds the lease."""
    run = client.run
    passed = []
    done = run('$name = "長頸鹿 \'a\' ""b"""\n"hello $name"\n(Get-Location).Path')
    assert done['state'] == 'succeeded' and done['exitCode'] == 0 and not done['outputTruncated'], done
    assert 'hello 長頸鹿 \'a\' "b"' in done['output'] and '\\Users\\' in done['output'], done
    passed.append('Several lines, quotes and non-ASCII text run as written, from the profile directory')
    assert run('cmd /c exit 7')['exitCode'] == 7
    assert run("cmd /c exit 7; 'fine'")['exitCode'] == 0
    assert run("Get-Process no-such-process -ErrorAction SilentlyContinue; 'fine'")['state'] == 'succeeded'
    thrown = run("'first'; throw 'stop here'; 'ne' + 'ver'")
    assert thrown['exitCode'] == 1 and 'first' in thrown['output'] and 'never' not in thrown['output'], thrown
    failed = run('Get-Item C:\\no-such-file-here')
    assert failed['state'] == 'failed' and failed['exitCode'] == 1 and 'no-such-file-here' in failed['output'], failed
    passed.append("A program's exit code is returned, and a failed last statement exits 1 with its error")
    started = time.monotonic()
    stopped = run("'before'; Start-Sleep -Seconds 60; 'after'", 3000)
    assert stopped['state'] == 'timed_out' and time.monotonic() - started < 20, stopped
    assert 'before' in stopped['output'] and 'after' not in stopped['output'], stopped
    assert client.call('windows_describe')['desktopBusy'] is False
    passed.append('A command is stopped at its deadline and what it printed is returned')
    long = run('1..20000 | ForEach-Object { "line $_" }', 60000)
    assert long['outputTruncated'] and len(long['output'].encode()) <= 64 * 1024, long['state']
    assert long['output'].rstrip().endswith('line 20000')
    passed.append('Long output keeps its end')
    def alive(pid: int) -> bool:
        seen = run(f'[bool](Get-Process -Id {pid} -ErrorAction SilentlyContinue); Stop-Process -Id {pid} -ErrorAction SilentlyContinue')
        return seen['output'].strip().splitlines()[0] == 'True'

    sleeper = "Start-Process powershell -ArgumentList '-NoProfile','-Command','Start-Sleep 60' -WindowStyle Hidden -PassThru"
    for program in (sleeper, 'Start-Process charmap -PassThru'):
        started = time.monotonic()
        launched = run(f'({program}).Id')
        assert time.monotonic() - started < 20, launched
        assert alive(int(launched['output'].strip().splitlines()[-1])), launched
    passed.append('A program started with Start-Process outlives the command')
    late = run(f'({sleeper}).Id; Start-Sleep -Seconds 60', 5000)
    assert late['state'] == 'timed_out' and not alive(int(late['output'].strip().splitlines()[0])), late
    passed.append('A command stopped at its deadline takes what it started with it')
    expect_error(lambda: run(' '), 'invalid_arguments')
    expect_error(lambda: run('x' * 32001), 'invalid_arguments')
    passed.append('An empty or oversized command is refused before anything runs')
    return passed


def check_lease(make) -> list[str]:
    """Who may run a command, and what ends one. `make()` gives a fresh client."""
    passed = []
    client = make()
    stranger = make()
    stranger.token = '0' * 64
    assert stranger.post('/mcp', {'jsonrpc': '2.0', 'id': 'x', 'method': 'tools/list', 'params': {}})[0] == 401
    assert sorted(tool['name'] for tool in client.tools()) == ['windows_describe', 'windows_exec_run']
    passed.append('A wrong token is refused, and the tool list is the two model-facing tools')

    assert client.describe()['controlOwner'] == 'human'
    expect_error(lambda: client.run("'no lease'"), 'human_control')
    passed.append('Without the lease nothing runs')

    with Lease(client) as held:
        time.sleep(7)  # longer than the lease: only heartbeats keep it
        assert held.run("'still held'")['output'].strip() == 'still held'
        other = make()
        other.describe()
        assert other.control('control.resume')['target'] == held.target
        passed.append('Heartbeats keep the lease, and a second connector joins it instead of taking it')

        result = {}
        long = make()
        long.target = held.target
        thread = threading.Thread(target=lambda: result.update(
            long.run("'started'; Start-Sleep -Seconds 60; 'finished'", 90000)))
        thread.start()
        time.sleep(4)
        taker = make()
        taker.target = held.target
        assert taker.control('control.takeover')['controlOwner'] == 'human'
        thread.join(20)
        assert result.get('state') == 'cancelled' and 'finished' not in result['output'], result
        expect_error(lambda: held.run("'after takeover'"), 'stale_session')
    passed.append('A takeover stops the running command and ends the lease')

    client.describe()
    client.control('control.resume')
    time.sleep(7)  # no heartbeats
    expect_error(lambda: client.run("'expired'"), 'stale_session')
    assert client.describe()['lastRevocation']['reason'] == 'expired'
    passed.append('A lease without heartbeats ends by itself')

    with Lease(client) as held:
        marker = '$env:TEMP\\wt-acceptance-marker.txt'
        impatient = make()
        impatient.target = held.target
        try:
            impatient.call('windows_exec_run', {'command': f"Remove-Item -ErrorAction SilentlyContinue {marker}; "
                                                f"Start-Sleep -Seconds 12; 'late' | Set-Content {marker}",
                                                'timeoutMs': 60000}, timeout=3)
            raise AssertionError('the caller should have stopped waiting')
        except HostError as error:
            assert error.code == 'transport_error', error
        until = time.monotonic() + 10
        while held.call('windows_describe')['desktopBusy'] and time.monotonic() < until:
            time.sleep(0.5)
        time.sleep(12)
        assert held.run(f'Test-Path {marker}')['output'].strip() == 'False'
    passed.append('A caller that stops waiting stops its command, and the lease stays')
    assert client.describe()['controlOwner'] == 'human' and client.describe()['desktopBusy'] is False
    return passed


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--url', required=True)
    parser.add_argument('--token-file', required=True, type=Path)
    parser.add_argument('--fingerprint', help="the certificate's SHA-256 as --prepare printed it; else the first seen")
    args = parser.parse_args(argv)
    if not __debug__:
        parser.error('Acceptance assertions require Python without -O.')
    token = args.token_file.read_text().strip()

    def make() -> Client:
        return Client(args.url, token, args.fingerprint)

    description = make().describe()
    if description.get('controlOwner') != 'human' or description.get('desktopBusy') is not False:
        parser.error('The host must be idle and human-owned before testing.')
    if description.get('interactiveDesktop') is not True:
        parser.error('The Windows session is locked or disconnected.')
    passed = check_lease(make)
    client = make()
    with Lease(client) as held:
        passed += check_run(held)
    print(json.dumps(passed, ensure_ascii=False, indent=1))


if __name__ == '__main__':
    main()
