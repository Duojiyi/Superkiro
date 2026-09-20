"""Read-only deployment identity, TLS, frontend parity, and customer-card checks."""
import hashlib
import json
from pathlib import Path
import sys
import requests
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from test_deployed_server import ROOT, HOST, PROXY, connect

config = json.load(sys.stdin)
release = json.loads((ROOT / 'deployment-candidate-results.json').read_text())
assert release['status'] == 'deployed'
ssh = connect(config['password'])
result = {'release': release['release'], 'checks': []}

def check(name, valid):
    assert valid, name
    result['checks'].append(name)
    print('PASS: ' + name, flush=True)

try:
    _, out, _ = ssh.exec_command("for i in $(seq 1 30); do [ \"$(docker inspect kiro-gateway --format '{{.State.Health.Status}}')\" = healthy ] && break; sleep 2; done; docker inspect kiro-gateway --format '{{.Config.Image}} {{.State.Health.Status}}'", timeout=90)
    check('New image running and healthy', out.read().decode().strip() == release['image'] + ' healthy')
    sftp = ssh.open_sftp()
    for name in ['gateway.env', 'caddy.env', 'root.crt']:
        old = sftp.open(release['backup'] + '/configuration/' + name).read()
        current = sftp.open('/etc/kiro-byok/' + name).read()
        check('Unchanged protected configuration: ' + name, old == current)
    web = json.loads(sftp.open('/etc/kiro-byok/admin-access.json').read())
    sftp.close()
    for proxy in [False, True]:
        session = requests.Session(); session.trust_env = False
        session.verify = str(ROOT / 'deploy/server-ca.pem')
        if proxy:
            session.proxies = {'http': PROXY, 'https': PROXY}
        mode = 'proxy' if proxy else 'direct'
        r = session.get('https://' + HOST + '/healthz', timeout=20)
        check(mode + ' TLS-verified health', r.status_code == 200)
        r = session.get('https://' + HOST + '/portal', timeout=20)
        check(mode + ' deployed portal matches source', r.status_code == 200 and r.content == (ROOT / 'apps/portal-ui/index.html').read_bytes())
        r = session.get('https://' + HOST + '/admin/', auth=('admin', web['password']), timeout=20)
        check(mode + ' deployed admin matches build', r.status_code == 200 and r.content == (ROOT / 'apps/admin-ui/dist/index.html').read_bytes())
        if not proxy:
            card = json.loads((ROOT / '.acceptance/private/manual-card.json').read_text(encoding='utf-8'))
            r = session.post('https://' + HOST + '/api/v1/portal/query', json={'card': card['card_key']}, timeout=20)
            r.raise_for_status(); data = r.json()
            before = json.loads((ROOT / '.acceptance/manual-card-before-release.json').read_text())
            after = {key: data[key] for key in before}
            check('Customer acceptance card unchanged and unused', after == before and after['status'] == 'unactivated' and not after['boundDevices'])
            result['manual_card'] = {'status': after['status'], 'points': after['remainingCredits'] / 1_000_000}
        session.close()
finally:
    ssh.close()
    (ROOT / 'deployment-candidate-verification.json').write_text(json.dumps(result, indent=2), encoding='utf-8')
