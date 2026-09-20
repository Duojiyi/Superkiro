"""Narrow live-source device reuse release. Credentials only via stdin."""
import difflib
import json
import re
import shlex
import sys
import uuid
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from deploy import release_candidate as rc

EXPECTED = '/opt/kiro-byok/releases/20260919T070809Z'
ENGINE = 'crates/billing/src/engine.rs'
TEST = 'crates/gateway/tests/auth_handshake_test.rs'
NAME = 'same_computer_can_use_new_card_without_sharing_card_across_devices'


def read(ssh, path):
    with ssh.open_sftp() as sftp:
        return sftp.open(path).read().decode()


def verify_live(ssh, report):
    base = 'https://kiro.rent'
    cards = []
    checks = []
    admin, public = rc.requests.Session(), rc.requests.Session()
    admin.trust_env = public.trust_env = False
    admin.headers['Origin'] = base

    def req(session, path, data=None, expected=200):
        response = session.request('GET' if data is None else 'POST', base + path,
                                   json=data, timeout=30, allow_redirects=False)
        if response.status_code != expected:
            raise RuntimeError('Verification HTTP status mismatch: ' + path + ' ' + str(response.status_code))
        return response

    def check(name, condition):
        if not condition:
            raise RuntimeError('Verification failed: ' + name)
        checks.append(name)
        print('PASS ' + name, flush=True)

    try:
        web = json.loads(read(ssh, '/etc/kiro-byok/admin-access.json'))
        req(admin, '/api/v1/admin/session', {'username': web['username'], 'password': web['password']})
        admin.headers['x-csrf-token'] = req(admin, '/api/v1/admin/session').json()['csrfToken']
        req(admin, '/api/v1/admin/cards')
        check('Administrator cookie session can access cards', True)
        for _ in range(2):
            issued = req(admin, '/api/v1/admin/cards/batch', {
                'count': 1, 'templateId': 'tier-1000', 'groupId': 'group-pro-plus',
                'note': 'Disposable device reuse verification ' + report['release']}).json()['cards']
            cards.extend(issued)
        check('Temporary cards enforce one device', all(c['maxDevices'] == 1 for c in cards))
        device = 'device-reuse-' + uuid.uuid4().hex
        for index in [0, 1, 0, 1]:
            token = req(public, '/oauth/token', {'card_key': cards[index]['rawCode'], 'device_id': device}).json()
            check('Card %s authentication on same device' % (index + 1), bool(token.get('accessToken')))
        for index, card in enumerate(cards):
            denied = public.post(base + '/oauth/token', json={'card_key': card['rawCode'], 'device_id': device + '-other'}, timeout=30)
            check('Card %s rejects another device' % (index + 1), denied.status_code == 403)
            state = req(public, '/api/v1/portal/query', {'card': card['rawCode']}).json()
            check('Card %s retains only original device' % (index + 1), state['boundDevices'] == [device])
    finally:
        cleanup = True
        for card in cards:
            try:
                req(admin, '/api/v1/admin/cards/status', {'cardId': card['cardId'], 'action': 'ban', 'reason': 'Device reuse verification complete'})
            except Exception:
                cleanup = False
        report['live_checks'] = checks
        report['temporary_cards_created'] = len(cards)
        report['temporary_cards_disabled'] = cleanup
        try:
            req(admin, '/api/v1/admin/session/revoke', {})
        finally:
            admin.close()
            public.close()
            rc.save_report(ssh, report)
        if not cleanup:
            raise RuntimeError('Temporary card cleanup needs attention')



def confirm_cleanup(ssh, report):
    """Fresh cookie session independently confirms persisted temporary-card status."""
    base = 'https://kiro.rent'
    with rc.requests.Session() as admin:
        admin.trust_env = False
        admin.headers['Origin'] = base

        def request(path, data=None):
            response = admin.request('GET' if data is None else 'POST', base + path,
                                     json=data, timeout=30, allow_redirects=False)
            if response.status_code != 200:
                raise RuntimeError('Cleanup verification HTTP status ' + str(response.status_code))
            return response.json()

        web = json.loads(read(ssh, '/etc/kiro-byok/admin-access.json'))
        request('/api/v1/admin/session', {'username': web['username'], 'password': web['password']})
        admin.headers['x-csrf-token'] = request('/api/v1/admin/session')['csrfToken']
        try:
            found = []
            offset = 0
            while True:
                page = request('/api/v1/admin/cards?limit=500&offset=' + str(offset))['cards']
                found.extend(card for card in page if re.fullmatch(
                    r'\[BANNED: Device reuse verification complete\] Disposable device reuse verification '
                    + re.escape(report['release']) + r'-#1', card.get('note') or ''))
                if len(page) < 500:
                    break
                offset += len(page)
            if len(found) != 2 or any(card['status'] != 'banned' for card in found):
                raise RuntimeError('Temporary card count or persisted banned status mismatch')
            report['temporary_card_cleanup_rechecked'] = {'count': len(found), 'all_banned': True}
        finally:
            request('/api/v1/admin/session/revoke', {})
    rc.save_report(ssh, report)
    (rc.ROOT / 'deploy/device-reuse-release-results.json').write_text(json.dumps(report, indent=2), encoding='utf8')
    print('PASS independent check: both temporary cards are banned', flush=True)


def main():
    ssh = rc.pinned_connection(json.load(sys.stdin))
    try:
        with rc.deployment_lock(ssh):
            old = rc.release_path(rc.run(ssh, 'readlink -f /opt/kiro-byok/current'))
            if old != EXPECTED:
                raise RuntimeError('Unexpected live source; no changes made')
            release = datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ')
            dest = rc.BASE + '/releases/' + release
            report = {'release': release, 'image': 'kiro-byok:' + release,
                      'backup': rc.BASE + '/backups/release-' + release, 'status': 'staging',
                      'previous_release': old, 'configuration_sha256': rc.configuration_digest(ssh, old),
                      'previous_image_id': rc.run(ssh, "docker inspect kiro-gateway --format '{{.Image}}'"),
                      'scope': [ENGINE, TEST, 'deploy/docker-compose.ip.yml']}
            paths = rc.SOURCE_PATHS + ['deploy/docker-compose.ip.yml', 'deploy/Caddyfile.ip']
            report['live_source_sha256'] = rc.tree_digest(ssh, old, paths)
            rc.run(ssh, f'mkdir {dest}\ncd {old}\ntar -cf - {shlex.join(paths)} | tar -xf - -C {dest}')
            if rc.tree_digest(ssh, dest, paths) != report['live_source_sha256']:
                raise RuntimeError('Live source copy differs')
            original = read(ssh, dest + '/' + ENGINE)
            guard = '''        if !device.is_empty()
            && candidate
                .cards
                .values()
                .any(|c| c.id != card_id && c.bound_devices.iter().any(|d| d == device))
        {
            return Err(BillingError::InvalidState(
                "device already bound to another card; explicitly unbind first".into(),
            ));
        }
'''
            patched = original
            for declaration in ['        let device = device_fp.unwrap_or("").trim();\n', '        let device = device_fp;\n']:
                block = declaration + guard
                if patched.count(block) != 1:
                    raise RuntimeError('Guard does not exactly match expected live code')
                patched = patched.replace(block, '', 1)
            rc.write_remote(ssh, dest + '/' + ENGINE, patched.encode())
            test_source = (rc.ROOT / TEST).read_text(encoding='utf8')
            marker = '#[tokio::test]\nasync fn ' + NAME + '()'
            test = test_source[test_source.index(marker):]
            if not test.rstrip().endswith('}') or test.count('#[tokio::test]') != 1:
                raise RuntimeError('Unexpected regression test boundaries')
            old_test = read(ssh, dest + '/' + TEST)
            if NAME in old_test:
                raise RuntimeError('Regression test already present')
            rc.write_remote(ssh, dest + '/' + TEST, (old_test.rstrip() + '\n\n' + test).encode())
            compose = read(ssh, dest + '/deploy/docker-compose.ip.yml')
            images = re.findall(r'(?m)^    image: (kiro-byok:[^\s]+)$', compose)
            if len(images) != 1:
                raise RuntimeError('Unexpected compose image')
            report['previous_image'] = images[0]
            rc.write_remote(ssh, dest + '/deploy/docker-compose.ip.yml', compose.replace('    image: ' + images[0], '    image: ' + report['image'], 1).encode())
            # Compare every copied input. Only the three explicit paths may differ.
            diff_script = '''import hashlib,json,pathlib,sys
old,new=map(pathlib.Path,sys.argv[1:3]); paths=json.loads(sys.argv[3])
def files(root):
 result={}
 for item in paths:
  p=root/item
  for f in ([p] if p.is_file() else p.rglob('*')):
   if f.is_file(): result[str(f.relative_to(root))]=hashlib.sha256(f.read_bytes()).hexdigest()
 return result
a,b=files(old),files(new)
assert a.keys()==b.keys()
print(json.dumps(sorted(k for k in a if a[k]!=b[k])))
'''
            changed = json.loads(rc.run(ssh, 'python3 -c ' + shlex.quote(diff_script) + ' ' + shlex.join([old, dest, json.dumps(paths)])))
            if changed != sorted(report['scope']):
                raise RuntimeError('Candidate changes exceed allowlist')
            report['verified_changed_paths'] = changed
            diff = ''.join(difflib.unified_diff(original.splitlines(True), patched.splitlines(True), fromfile='live/' + ENGINE, tofile='candidate/' + ENGINE))
            (rc.ROOT / 'deploy/device-reuse-live.patch').write_text(diff, encoding='utf8')
            report['candidate_sha256'] = rc.tree_digest(ssh, dest, paths)
            rc.save_report(ssh, report)
            print('STAGED ' + release + ': exact allowlist verified', flush=True)
            script = f'''set -eu
cd {dest}
if docker build --iidfile build.image-id -t {report['image']} -f deploy/Dockerfile . >build.log 2>&1; then code=0; else code=$?; fi
printf '%s' "$code" >build.exit
[ "$code" = 0 ]
docker build --target builder -t kiro-device-test:{release} -f deploy/Dockerfile . >test-build.log 2>&1
if docker run --rm -e CARGO_BUILD_JOBS=1 kiro-device-test:{release} cargo test --locked -p gateway --test auth_handshake_test >test.log 2>&1; then code=0; else code=$?; fi
printf '%s' "$code" >test.exit
exit "$code"
'''
            rc.write_remote(ssh, dest + '/limited-build.sh', script.encode())
            rc.run(ssh, 'sh ' + dest + '/limited-build.sh', timeout=7200)
            report['auth_handshake_tests'] = rc.run(ssh, f"grep '^test result:' {dest}/test.log")
            rc.save_report(ssh, report)
            print('PASS server Docker build and auth handshake tests', flush=True)
            rc.promote(ssh, report)
            print('DEPLOYED ' + release, flush=True)
            verify_live(ssh, report)
            confirm_cleanup(ssh, report)
            rc.wait_gateway(ssh, report['image_id'])
            rc.external_readiness()
            report['final_current'] = rc.run(ssh, 'readlink -f /opt/kiro-byok/current')
            if report['final_current'] != dest:
                raise RuntimeError('Final release mismatch')
            report['verification'] = 'passed'
            rc.save_report(ssh, report)
            (rc.ROOT / 'deploy/device-reuse-release-results.json').write_text(json.dumps(report, indent=2), encoding='utf8')
            print('COMPLETE ' + release, flush=True)
    finally:
        ssh.close()


if __name__ == '__main__':
    main()
