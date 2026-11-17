#!/usr/bin/env python3
"""Offline release-binary HTTP/MCP smoke probe. Uses synthetic local-only config."""
import json
import os
import pathlib
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

binary = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else 'target/release/rust-microsoft-todo-mcp').resolve()
with tempfile.TemporaryDirectory(prefix='todo-smoke-') as data:
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    origin = f'http://127.0.0.1:{port}'
    token = 'synthetic-local-mcp-token-0123456789'
    env = os.environ | {
        'TODO_BIND': f'127.0.0.1:{port}', 'TODO_PUBLIC_URL': origin,
        'TODO_DATA_DIR': data, 'TODO_DEV_MCP_TOKEN': token,
        'TODO_ADMIN_TOKEN': 'synthetic-admin-token-01234567890123',
        'TODO_ENCRYPTION_KEY': '01' * 32,
        'MICROSOFT_CLIENT_ID': '00000000-0000-0000-0000-000000000001',
        'RUST_LOG': 'warn',
    }
    start = time.perf_counter()
    process = subprocess.Popen([str(binary)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    try:
        for _ in range(500):
            if process.poll() is not None:
                raise RuntimeError(process.stderr.read().decode())
            try:
                with urllib.request.urlopen(origin + '/healthz', timeout=.2) as response:
                    assert json.load(response)['status'] == 'ok'
                break
            except (OSError, urllib.error.URLError):
                time.sleep(.01)
        else:
            raise TimeoutError('health check did not become ready')
        startup_ms = (time.perf_counter() - start) * 1000
        try:
            urllib.request.urlopen(origin + '/mcp', timeout=2)
            raise AssertionError('unauthenticated request accepted')
        except urllib.error.HTTPError as error:
            assert error.code == 401
            assert 'resource_metadata' in error.headers['WWW-Authenticate']
        def rpc(method, params):
            request = urllib.request.Request(origin + '/mcp', data=json.dumps({'jsonrpc': '2.0', 'id': 1, 'method': method, 'params': params}).encode(), headers={
                'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json',
                'Accept': 'application/json, text/event-stream', 'MCP-Protocol-Version': '2025-11-25'})
            with urllib.request.urlopen(request, timeout=5) as response:
                raw = response.read()
                return json.loads(raw), len(raw)
        initialized, _ = rpc('initialize', {'protocolVersion': '2025-11-25', 'capabilities': {}, 'clientInfo': {'name': 'offline-smoke', 'version': '1'}})
        assert 'tools' in initialized['result']['capabilities']
        discovery, discovery_bytes = rpc('tools/list', {})
        accounts, accounts_bytes = rpc('tools/call', {'name': 'list_accounts', 'arguments': {}})
        assert accounts['result']['structuredContent']['data']['accounts'] == []
        rss_kib = None
        status_file = pathlib.Path(f'/proc/{process.pid}/status')
        if status_file.exists():
            rss_kib = int(next(line for line in status_file.read_text().splitlines() if line.startswith('VmRSS:')).split()[1])
        print(json.dumps({'startup_to_health_ms': round(startup_ms, 3), 'idle_rss_kib': rss_kib, 'tool_count': len(discovery['result']['tools']), 'tools_list_wire_bytes': discovery_bytes, 'list_accounts_wire_bytes': accounts_bytes, 'graph_requests': 0, 'checks': ['health', 'unauthorized_401', 'mcp_initialize', 'tools_list', 'tools_call']}, indent=2))
    finally:
        process.terminate()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
