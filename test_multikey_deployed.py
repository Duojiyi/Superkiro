"""Read-only production key discovery and sanitized request-trace verification.
SSH credentials arrive through stdin. Never stores credentials or changes permissions.
"""
import json, sys
import requests
from test_deployed_server import connect, ROOT, HOST

def main():
    ssh = connect(json.load(sys.stdin)['password'], False)
    session = requests.Session()
    session.trust_env = False
    session.verify = str(ROOT / 'deploy/server-ca.pem')
    result = {}
    try:
        with ssh.open_sftp() as sftp:
            env = dict(line.split('=', 1) for line in sftp.open('/etc/kiro-byok/gateway.env').read().decode().splitlines() if '=' in line)
        def req(path, payload=None, headers=None):
            response = session.request('GET' if payload is None else 'POST', 'https://' + HOST + path,
                json=payload, headers=headers, timeout=(15, 45), allow_redirects=False)
            assert response.status_code == 200, f'{path}: HTTP {response.status_code}'
            return response.json()
        admin = {'Authorization': 'Bearer ' + req('/api/v1/admin/session', {}, {'x-admin-key': env['ADMIN_KEY']})['accessToken']}
        before = req('/api/v1/admin/providers', headers=admin)
        keys = before['keys']
        assert all(not key.get('api_key') and not key.get('api_key_encrypted') for key in keys)
        key = next(key for key in keys if key['provider_id'] == 'kimera-primary' and key['enabled'])
        config_before = req('/api/v1/admin/commercial-config', headers=admin)
        discovery = req('/api/v1/admin/providers/keys/discover', {'provider_id': key['provider_id'], 'key_id': key['id']}, admin)
        assert discovery['success'] and discovery['published'] is False
        assert discovery['models'] == sorted(set(discovery['models']))
        after = req('/api/v1/admin/providers', headers=admin)
        assert before == after, 'Discovery mutated providers or permissions'
        assert config_before == req('/api/v1/admin/commercial-config', headers=admin), 'Discovery mutated published configuration'
        result = {'discovery_count': len(discovery['models']), 'has_more': discovery['has_more'], 'discovery_no_mutation': True, 'secrets_redacted': True}
        traces = req('/api/v1/admin/traces?limit=100', headers=admin)['traces']
        routed = [t for t in traces if t.get('attempt_chain')]
        assert routed, 'No new routed trace available; run disposable-card acceptance first'
        assert any(t['status'] == 'success' and t['credits_charged'] > 0 for t in routed)
        assert all(len(t['attempt_chain']) <= 3 for t in routed)
        result['routed_traces'] = len(routed)
        result['successful_billed_trace'] = True
        result['recorded_attempts_within_budget'] = True
        print(json.dumps(result, ensure_ascii=False), flush=True)
    finally:
        (ROOT / '.acceptance/multikey-deployed-results.json').write_text(json.dumps(result, indent=2), encoding='utf-8')
        session.close()
        ssh.close()

if __name__ == '__main__':
    main()
