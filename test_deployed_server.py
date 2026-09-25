"""Local-to-server HTTPS E2E. Creates and finally bans one test card.
Credentials are read over pinned SSH, never saved locally. Small real API calls incur usage.
Run: python test_deployed_server.py (prompts for the SSH password).
Use --stdin-config to supply {"password": ...} over stdin for automation.
"""
import base64
import getpass
import hashlib
import json
from pathlib import Path
import re
import socket
import sys
import time
import uuid

import paramiko
import requests
from test_single_server_smoke import decode_frames

ROOT = Path(__file__).resolve().parent
HOST = '160.202.47.98'
PROXY = 'http://127.0.0.1:7897'
FINGERPRINT = 'sTYVluUx3J9nai3Cj67JvjQ5+DRuqLjjiWXT1s+wyiI'


def connect(password, use_proxy=True):
    sock = None
    transport = None
    try:
        if not use_proxy:
            sock = socket.create_connection((HOST, 22), 15)
        else:
            sock = socket.create_connection(('127.0.0.1', 7897), 15)
            sock.sendall(f'CONNECT {HOST}:22 HTTP/1.1\r\nHost: {HOST}:22\r\n\r\n'.encode())
            reply = b''
            while not reply.endswith(b'\r\n\r\n'):
                part = sock.recv(1)
                if not part or len(reply) > 8192:
                    raise RuntimeError('Invalid SSH proxy response')
                reply += part
            if b' 200 ' not in reply.split(b'\r\n')[0]:
                raise RuntimeError('SSH proxy rejected tunnel')
        transport = paramiko.Transport(sock)
        transport.start_client(timeout=20)
        fingerprint = base64.b64encode(hashlib.sha256(transport.get_remote_server_key().asbytes()).digest()).decode().rstrip('=')
        # Security checks must also run under python -O, before sending credentials.
        if fingerprint != FINGERPRINT:
            raise RuntimeError('SSH host key changed')
        transport.auth_password('root', password)
        transport.set_keepalive(20)
        client = paramiko.SSHClient()
        client._transport = transport
        return client
    except Exception:
        if transport is not None:
            transport.close()
        elif sock is not None:
            sock.close()
        raise


def main():
    if not __debug__:
        raise SystemExit('Run without -O: these checks are assertions')
    config = json.load(sys.stdin) if '--stdin-config' in sys.argv else {'password': getpass.getpass('SSH password: ')}
    ssh = connect(config['password'])
    results = []
    card = None
    admin = None
    session = requests.Session()
    session.trust_env = False
    session.proxies = {'http': PROXY, 'https': PROXY}

    def shell(command):
        _, stdout, stderr = ssh.exec_command(command, timeout=90)
        output = stdout.read().decode()
        errors = stderr.read().decode()
        assert stdout.channel.recv_exit_status() == 0, 'Remote command failed: ' + errors[:160]
        return output

    sftp = ssh.open_sftp()
    env = dict(line.split('=', 1) for line in sftp.open('/etc/kiro-byok/gateway.env').read().decode().splitlines() if '=' in line)
    web = json.loads(sftp.open('/etc/kiro-byok/admin-access.json').read())
    ca = ROOT / 'deploy' / 'server-ca.pem'
    sftp.get('/etc/kiro-byok/root.crt', str(ca))
    sftp.close()
    session.verify = str(ca)
    base = 'https://' + HOST

    def check(name, condition=True):
        assert condition, name
        results.append({'check': name, 'passed': True})
        print('PASS: ' + name, flush=True)

    def req(path, data=None, headers=None, expected=200, method=None, auth=None):
        r = session.request(method or ('GET' if data is None else 'POST'), base + path,
                            json=data, headers=headers, auth=auth, timeout=(15, 90), allow_redirects=False)
        assert r.status_code == expected, f'{path}: expected {expected}, got {r.status_code}'
        return r

    try:
        req('/healthz')
        check('HTTPS certificate validation and health')
        req('/admin/', expected=401)
        ui = req('/admin/', auth=('admin', web['password']))
        check('Admin UI Basic Auth and HTML', '<div id="root">' in ui.text)
        assets = re.findall(r'(?:src|href)="([^"]+\.(?:js|css))"', ui.text)
        assert assets, 'No frontend assets found'
        for asset in assets:
            req(asset if asset.startswith('/') else '/admin/' + asset, auth=('admin', web['password']))
        check('Admin JS and CSS assets')
        req('/api/v1/admin/stats', expected=401)
        req('/api/v1/cards/pull', {'order_id': 'unauthorized', 'count': 1}, expected=401)
        req('/generateAssistantResponse', {}, expected=401)
        check('Unauthenticated admin, card and inference denied')
        token = req('/api/v1/admin/session', {}, {'x-admin-key': env['ADMIN_KEY']}).json()['accessToken']
        admin = {'Authorization': 'Bearer ' + token}
        for route in ('me', 'stats', 'cards', 'providers', 'financials', 'traces', 'announcements', 'exports/ledger.json', 'exports/ledger.csv'):
            req('/api/v1/admin/' + route, headers=admin)
        check('Admin session and nine read/export routes')
        req('/api/v1/admin/commercial-config', expected=401)
        config = req('/api/v1/admin/commercial-config', headers=admin).json()['config']
        probe_id = 'deployment-probe-' + uuid.uuid4().hex
        now = int(time.time())
        publication = {
            'expected_revision': config['revision'],
            'reason': 'Isolated deployment acceptance; no customer group or mapping changed',
            'rate_cards': [{'id': probe_id, 'name': 'Isolated acceptance rate', 'created_at_secs': now}],
            'groups': [{'id': probe_id, 'name': 'Isolated acceptance group (no customer cards)',
                'provider_binding_mode': 'shared', 'rate_card_id': probe_id, 'margin_multiplier': 1.0,
                'virtual_plan_name': 'Acceptance only', 'virtual_usage_limit': 0, 'system_prompt_prefix': None}],
            'versions': [{'id': probe_id + '-v1', 'rate_card_id': probe_id, 'model': '*',
                'currency': 'CNY', 'pricing_mode': 'per_call', 'input_price_per_m': 0,
                'output_price_per_m': 0, 'cache_creation_price_per_m': 0, 'cache_read_price_per_m': 0,
                'fixed_input_credit_per_m': 0, 'fixed_output_credit_per_m': 0,
                'fixed_cache_creation_credit_per_m': 0, 'fixed_cache_read_credit_per_m': 0,
                'per_call_credit': 1000, 'margin_multiplier': 1.0, 'effective_from_secs': now + 300}]
        }
        published = req('/api/v1/admin/commercial-config', publication, admin).json()['config']
        check('Atomic group and scheduled pricing publication',
              published['revision'] != config['revision'] and len(published['audit']) == len(config['audit']) + 1)
        req('/api/v1/admin/commercial-config', publication, admin, expected=409)
        check('Stale publication rejected without changing customer configuration', published['models'] == config['models'])
        order = 'deployment-e2e-' + uuid.uuid4().hex
        platform = {'X-Card-Platform-Key': env['CARD_PLATFORM_KEY']}
        issued = req('/api/v1/cards/pull', {'order_id': order, 'count': 1}, platform).json()
        card = issued['cards'][0]
        again = req('/api/v1/cards/pull', {'order_id': order, 'count': 1}, platform).json()
        check('Card issuance is idempotent', again['cards'][0]['card_id'] == card['card_id'])
        code = card['raw_code']
        auths = []
        for device in ('e2e-device-A', 'e2e-device-B'):
            auths.append(req('/oauth/token', {'card_key': code, 'device_id': device}).json())
        check('Two devices can authenticate')
        old_refresh = auths[0]['refreshToken']
        for i in (0, 1, 0, 1):
            auths[i] = req('/refreshToken', {'refreshToken': auths[i]['refreshToken']}).json()
        req('/refreshToken', {'refreshToken': old_refresh}, expected=401)
        check('Alternating device refresh and refresh replay rejection')
        auth = {'Authorization': 'Bearer ' + auths[0]['accessToken']}
        quota = req('/getUsageLimits', headers=auth).json()
        req('/listAvailableSubscriptions', {}, auth)
        req('/ListAvailableModels', headers=auth)
        for method in ('initialize', 'tools/list'):
            rpc = req('/mcp', {'jsonrpc': '2.0', 'id': 1, 'method': method}, auth).json()
            assert 'result' in rpc, 'MCP protocol response missing result'
        req('/GenerateCompletions', {}, auth, expected=429)
        check('Usage, subscriptions, MCP handshake and explicit autocomplete throttling')
        req('/client/negotiate', {})
        req('/portal')
        private = session.post(base + '/api/v1/portal/query', json={'card': card['card_id']}, timeout=20)
        check('Public card ID cannot query private portal data', private.status_code != 200)
        balance = lambda: req('/api/v1/portal/query', {'card': code}).json()
        before = balance()
        check('Kiro quota equals purchased card points and disables overage',
              quota['usageBreakdownList'][0]['usageLimit'] == before['totalCredits'] / 1_000_000
              and quota['overageConfiguration']['overageEnabled'] is False)
        payload = {'conversationState': {'conversationId': order, 'history': [],
            'currentMessage': {'userInputMessage': {'content': 'Reply only with OK.', 'modelId': 'claude-sonnet-4-6'}}}}
        invocation = uuid.uuid4().hex
        r = req('/generateAssistantResponse', payload, {**auth, 'amz-sdk-invocation-id': invocation})
        frames = decode_frames(r.content)
        check('Real Kimera stream converted to valid AWS EventStream frames',
              'application/vnd.amazon.eventstream' in r.headers['Content-Type'] and any('OK' in str(f) for f in frames))
        after = balance()
        check('Real model usage debits credits', after['remainingCredits'] < before['remainingCredits'])
        duplicate = session.post(base + '/generateAssistantResponse', json=payload,
            headers={**auth, 'amz-sdk-invocation-id': invocation}, timeout=90)
        assert duplicate.status_code == 200, 'Duplicate replay response failed'
        check('Duplicate invocation does not debit twice', balance()['remainingCredits'] == after['remainingCredits'])
        tool_payload = {'conversationState': {'conversationId': order + '-tool', 'history': [],
            'currentMessage': {'userInputMessage': {
                'content': 'Call the echo_probe tool exactly once with value OK. Do not answer in text.',
                'modelId': 'claude-sonnet-4-6',
                'userInputMessageContext': {'tools': [{'toolSpecification': {
                    'name': 'echo_probe', 'description': 'Return the supplied value. Harmless deployment probe.',
                    'inputSchema': {'type': 'object', 'properties': {'value': {'type': 'string'}}, 'required': ['value']}
                }}]}
            }}}}
        tool_response = req('/generateAssistantResponse', tool_payload,
            {**auth, 'amz-sdk-invocation-id': uuid.uuid4().hex})
        tool_frames = decode_frames(tool_response.content)
        check('Real model tool schema and tool-use EventStream', any('echo_probe' in str(f) for f in tool_frames))
        after = balance()
        req('/api/v1/admin/cards/status', {'cardId': card['card_id'], 'action': 'freeze', 'reason': 'deployment E2E'}, admin)
        blocked = session.get(base + '/getUsageLimits', headers=auth, timeout=20)
        check('Frozen card access rejected', blocked.status_code in (401, 403))
        req('/api/v1/admin/cards/status', {'cardId': card['card_id'], 'action': 'unfreeze', 'reason': 'deployment E2E'}, admin)
        check('Card freeze/unfreeze administration')
        req('/api/v1/admin/snapshot/sync', {}, admin)
        state_format = shell("python3 -c \"import json;print(json.load(open('/opt/kiro-byok/data/billing_state.json'))['format'])\"").strip()
        check('Production snapshot is AEAD encrypted', state_format == 'kiro-billing-aead-v1')
        shell('docker restart kiro-gateway >/dev/null')
        for _ in range(40):
            try:
                if session.get(base + '/healthz', timeout=5).status_code == 200:
                    break
            except requests.RequestException:
                pass
            time.sleep(1)
        restored = balance()
        check('Restart preserves balance and device bindings',
              restored['remainingCredits'] == after['remainingCredits'] and restored['boundDevices'] == after['boundDevices'])
        token = req('/api/v1/admin/session', {}, {'x-admin-key': env['ADMIN_KEY']}).json()['accessToken']
        admin = {'Authorization': 'Bearer ' + token}
        reloaded_config = req('/api/v1/admin/commercial-config', headers=admin).json()['config']
        check('Restart preserves published groups, prices and audit',
              reloaded_config['revision'] == published['revision'] and reloaded_config['audit'] == published['audit'])
        req('/api/v1/admin/session/revoke', {}, admin)
        req('/api/v1/admin/stats', headers=admin, expected=401)
        check('Admin session revocation')
    except Exception as exc:
        results.append({'check': 'workflow', 'passed': False, 'error': str(exc)})
        print('FAIL: ' + str(exc), flush=True)
        raise
    finally:
        if card:
            try:
                token = req('/api/v1/admin/session', {}, {'x-admin-key': env['ADMIN_KEY']}).json()['accessToken']
                req('/api/v1/admin/cards/status', {'cardId': card['card_id'], 'action': 'ban', 'reason': 'deployment E2E finished'}, {'Authorization': 'Bearer ' + token})
                print('Test card banned; audit trail retained.', flush=True)
            except Exception:
                print('WARNING: test card cleanup requires administrator review.', flush=True)
        (ROOT / 'deployment-e2e-results.json').write_text(json.dumps(results, indent=2), encoding='utf-8')
        ssh.close()
        session.close()


if __name__ == '__main__':
    main()
