"""Real-process gateway smoke test with an isolated, deterministic SSE upstream.

Run: python test_single_server_smoke.py
Builds the current gateway before testing. No production data or credentials used.
"""
import json
import os
from pathlib import Path
import secrets
import socket
import struct
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request
import zlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = Path(__file__).resolve().parent


class Upstream(BaseHTTPRequestHandler):
    calls = 0

    def log_message(self, *_):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        if self.path != '/v1/chat/completions' or not body.get('stream'):
            self.send_error(400)
            return
        type(self).calls += 1
        chunks = [
            {'choices': [{'index': 0, 'delta': {'content': 'Hello from smoke upstream'}, 'finish_reason': None}]},
            {'choices': [{'index': 0, 'delta': {}, 'finish_reason': 'stop'}],
             'usage': {'prompt_tokens': 150, 'completion_tokens': 40, 'total_tokens': 190}},
        ]
        data = ''.join('data: ' + json.dumps(c) + '\n\n' for c in chunks) + 'data: [DONE]\n\n'
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.send_header('Content-Length', str(len(data.encode())))
        self.end_headers()
        self.wfile.write(data.encode())


def request(base, path, payload=None, headers=None):
    req = urllib.request.Request(base + path,
        data=None if payload is None else json.dumps(payload).encode(),
        headers={'Content-Type': 'application/json', **(headers or {})})
    try:
        with urllib.request.urlopen(req, timeout=15) as response:
            return response.status, response.headers, response.read()
    except urllib.error.HTTPError as error:
        return error.code, error.headers, error.read()


def decode_frames(data):
    payloads = []
    while data:
        if len(data) < 16:
            raise ValueError('Truncated eventstream frame')
        total, headers_length, prelude_crc = struct.unpack('!III', data[:12])
        if not 16 <= total <= len(data):
            raise ValueError('Invalid frame length')
        frame, data = data[:total], data[total:]
        if zlib.crc32(frame[:8]) != prelude_crc:
            raise ValueError('Invalid prelude CRC')
        if zlib.crc32(frame[:-4]) != struct.unpack('!I', frame[-4:])[0]:
            raise ValueError('Invalid message CRC')
        payloads.append(json.loads(frame[12 + headers_length:-4]))
    return payloads


def stop(proc):
    if proc is not None and proc.poll() is None:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=5)


def main():
    if not __debug__:
        raise SystemExit('Run without -O: these checks are assertions')
    subprocess.run(['cargo', 'build', '--locked', '-p', 'gateway', '--bin', 'gateway'], cwd=ROOT, check=True)
    exe = ROOT / 'target' / 'debug' / ('gateway.exe' if os.name == 'nt' else 'gateway')
    upstream = ThreadingHTTPServer(('127.0.0.1', 0), Upstream)
    thread = threading.Thread(target=upstream.serve_forever, daemon=True)
    thread.start()
    proc = None
    try:
        with tempfile.TemporaryDirectory(prefix='kiro_smoke_') as directory:
            with socket.socket() as sock:
                sock.bind(('127.0.0.1', 0))
                port = sock.getsockname()[1]
            base = f'http://127.0.0.1:{port}'
            # Avoid inheriting production configuration, secret-file paths, or proxy settings.
            env = {k: v for k, v in os.environ.items() if k.upper() in {
                'PATH', 'SYSTEMROOT', 'WINDIR', 'TEMP', 'TMP', 'HOME', 'USERPROFILE', 'APPDATA', 'LOCALAPPDATA'}}
            env.update(HOST='127.0.0.1', PORT=str(port), DATA_DIR=directory,
                AUTH_SECRET=secrets.token_hex(32), ADMIN_KEY=secrets.token_hex(32),
                CARD_PLATFORM_KEY=secrets.token_hex(32), KIRO_MASTER_KEK=secrets.token_hex(32),
                REQUIRE_ENCRYPTED_SNAPSHOTS='true', PROVIDER_TYPE='openai',
                UPSTREAM_BASE_URL=f'http://127.0.0.1:{upstream.server_port}',
                UPSTREAM_API_KEY='isolated-test-key', UPSTREAM_MODEL='smoke-model')
            with tempfile.TemporaryFile(mode='w+b') as log:
                def start():
                    child = subprocess.Popen([str(exe)], cwd=ROOT, env=env, stdout=log, stderr=log)
                    try:
                        deadline = time.monotonic() + 15
                        while time.monotonic() < deadline:
                            if child.poll() is not None:
                                raise RuntimeError('Gateway exited during startup')
                            try:
                                if request(base, '/healthz')[0] == 200:
                                    return child
                            except OSError:
                                pass
                            time.sleep(.1)
                        raise RuntimeError('Gateway startup timed out')
                    except Exception:
                        stop(child)
                        log.seek(0)
                        print(log.read().decode(errors='replace'))
                        raise

                try:
                    proc = start()
                    assert request(base, '/client/negotiate', {})[0] == 200
                    assert request(base, '/api/v1/cards/pull', {'order_id': 'unauthorized', 'count': 1})[0] == 401
                    assert request(base, '/api/v1/admin/cards')[0] == 401
                    print('PASS: startup, negotiation, card and admin authentication')
                    status, _, body = request(base, '/api/v1/cards/pull', {'order_id': 'smoke-order', 'count': 1},
                        {'X-Card-Platform-Key': env['CARD_PLATFORM_KEY']})
                    assert status == 200, (status, body)
                    card = json.loads(body)['cards'][0]
                    code = card['raw_code']
                    status, _, body = request(base, '/oauth/token', {'card_key': code, 'device_id': 'smoke-device'})
                    assert status == 200, (status, body)
                    auth = {'Authorization': 'Bearer ' + json.loads(body)['accessToken']}
                    assert request(base, '/getUsageLimits', headers=auth)[0] == 200
                    assert request(base, '/portal')[0] == 200
                    assert request(base, '/api/v1/portal/query', {'card': card['card_id']})[0] != 200

                    def balance():
                        status, _, body = request(base, '/api/v1/portal/query', {'card': code})
                        assert status == 200, (status, body)
                        return json.loads(body)

                    before = balance()
                    assert before['status'] == 'active'
                    print('PASS: card issuance, login, usage and secret-only portal lookup')
                    payload = {'conversationState': {'conversationId': 'smoke-conversation', 'history': [],
                        'currentMessage': {'userInputMessage': {'content': 'Say hello', 'modelId': 'smoke-model'}}}}
                    status, headers, body = request(base, '/generateAssistantResponse', payload,
                        {**auth, 'amz-sdk-invocation-id': 'smoke-invocation'})
                    assert status == 200, (status, body)
                    assert 'application/vnd.amazon.eventstream' in headers['Content-Type']
                    frames = decode_frames(body)
                    assert any('Hello from smoke upstream' in str(frame) for frame in frames), frames
                    after = balance()
                    assert after['remainingCredits'] < before['remainingCredits'], (before, after)
                    assert Upstream.calls == 1
                    print('PASS: real HTTP upstream, SSE to CRC-validated eventstream, billing debit')
                    snapshot = Path(directory) / 'billing_state.json'
                    assert snapshot.exists()
                    assert code not in snapshot.read_text(encoding='utf-8')
                    stop(proc)
                    proc = start()
                    restored = balance()
                    assert restored['cardId'] == card['card_id']
                    assert restored['remainingCredits'] == after['remainingCredits']
                    assert restored['boundDevices'] == after['boundDevices']
                    assert request(base, '/getUsageLimits', headers=auth)[0] == 200
                    print('PASS: encrypted state survives process restart, balance and device binding preserved')
                finally:
                    stop(proc)
    finally:
        stop(proc)
        upstream.shutdown()
        upstream.server_close()
        thread.join(timeout=5)
    print('ALL REAL-PROCESS SMOKE CHECKS PASSED (mock upstream; not production E2E)')


if __name__ == '__main__':
    main()
