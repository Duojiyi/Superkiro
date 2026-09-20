"""Actual Kiro core network-stack regression; no IDE launch or customer card use.
Optional --authenticated issues and finally bans an isolated server test card.
SSH password travels over stdin as JSON, never written to disk.
"""
import json, os, subprocess, sys, uuid
from pathlib import Path
from test_deployed_server import ROOT, HOST, connect
import requests

def main():
    admin = card = None
    session = requests.Session()
    session.trust_env = False
    session.verify = str(ROOT / 'deploy/server-ca.pem')
    def req(path, **kw):
        response = session.request(kw.pop('method', 'POST'), 'https://' + HOST + path, timeout=25, **kw)
        response.raise_for_status()
        return response.json()
    try:
        credentials = {}
        if '--authenticated' in sys.argv:
            ssh = connect(json.load(sys.stdin)['password'])
            try:
                with ssh.open_sftp() as sftp:
                    env = dict(line.split('=', 1) for line in sftp.open('/etc/kiro-byok/gateway.env').read().decode().splitlines() if '=' in line)
            finally:
                ssh.close()
            admin = {'Authorization': 'Bearer ' + req('/api/v1/admin/session', headers={'x-admin-key': env['ADMIN_KEY']}, json={})['accessToken']}
            card = req('/api/v1/cards/pull', headers={'X-Card-Platform-Key': env['CARD_PLATFORM_KEY']}, json={'order_id': 'core-proxy-regression-' + uuid.uuid4().hex, 'count': 1})['cards'][0]
            credentials['accessToken'] = req('/oauth/token', json={'card_key': card['raw_code'], 'device_id': 'isolated-core-proxy-regression'})['accessToken']
        output = ROOT / '.acceptance/kiro-core-proxy-result.json'
        runtime = {**os.environ, 'ELECTRON_RUN_AS_NODE': '1', 'NODE_PATH': os.path.expandvars(r'%LOCALAPPDATA%/Programs/Kiro/resources/app/node_modules.asar'), 'KIRO_PROXY_TEST_OUTPUT': str(output)}
        command = [os.path.expandvars(r'%LOCALAPPDATA%/Programs/Kiro/Kiro.exe'), str(ROOT / 'test_kiro_core_proxy.cjs')]
        if credentials: command.append('--authenticated')
        result = subprocess.run(command, input=json.dumps(credentials).encode(), env=runtime, capture_output=True, timeout=60, cwd=ROOT)
        report = json.loads(output.read_text(encoding='utf8'))
        print(json.dumps(report, ensure_ascii=False, indent=2))
        assert result.returncode == 0 and report['success'], 'Kiro core proxy regression failed'
    finally:
        if card:
            req('/api/v1/admin/cards/status', headers=admin, json={'cardId': card['card_id'], 'action': 'ban', 'reason': 'core proxy regression cleanup'})
            print('Isolated test card banned; customer card untouched.')
        session.close()
if __name__ == '__main__': main()
