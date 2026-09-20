"""Release audited web assets and the public announcement endpoint over pinned live source."""
import hashlib
import difflib
import json
import re
import shlex
import sys
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from deploy import release_candidate as rc

EXPECTED = '/opt/kiro-byok/releases/20260919T130652Z'
CLIENT = 'crates/gateway/src/facade/client.rs'
REGISTRY = 'crates/gateway/src/facade/mod.rs'
TEST = 'crates/gateway/tests/announcements_test.rs'

def read(ssh, path):
    with ssh.open_sftp() as sftp:
        return sftp.open(path).read().decode()

def verify_public_release(report):
    with rc.requests.Session() as public:
        public.trust_env = False
        response = public.get('https://kiro.rent/api/v1/announcements', timeout=20)
        response.raise_for_status()
        payload = response.json()
        if payload.get('success') is not True or not isinstance(payload.get('announcements'), list):
            raise RuntimeError('Public announcements readiness failed')
        for item in payload['announcements']:
            if set(item) != {'id','level','title','content','created_at','expires_at'}:
                raise RuntimeError('Public field allowlist mismatch')
        for url, digest in report['web_sha256'].items():
            response = public.get('https://kiro.rent' + url, timeout=20)
            response.raise_for_status()
            if hashlib.sha256(response.content).hexdigest() != digest:
                raise RuntimeError('Public asset mismatch: ' + url)
    report['public_announcements_verified'] = True
    report['public_assets_verified'] = True


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
                      'scope': [CLIENT, REGISTRY, TEST, 'deploy/docker-compose.ip.yml']}
            paths = rc.SOURCE_PATHS + ['deploy/docker-compose.ip.yml', 'deploy/Caddyfile.ip']
            report['live_source_sha256'] = rc.tree_digest(ssh, old, paths)
            rc.run(ssh, f'mkdir {dest}\ncd {old}\ntar -cf - {shlex.join(paths)} | tar -xf - -C {dest}')
            if rc.tree_digest(ssh, dest, paths) != report['live_source_sha256']:
                raise RuntimeError('Live source copy differs')
            original = read(ssh, dest + '/' + CLIENT)
            marker = '/// Public, read-only announcements;'
            addition = (rc.ROOT / CLIENT).read_text(encoding='utf8').split(marker, 1)[1]
            if 'pub struct AnnouncementsHandler' in original:
                raise RuntimeError('Public announcement handler already present')
            patched = original.rstrip() + '\n\n' + marker + addition
            rc.write_remote(ssh, dest + '/' + CLIENT, patched.encode())
            registry = read(ssh, dest + '/' + REGISTRY)
            anchor = '        self.register(portal::PortalChallengeHandler {'
            if registry.count(anchor) != 1:
                raise RuntimeError('Registry anchor mismatch')
            registry = registry.replace(anchor, '        self.register(client::AnnouncementsHandler {\n            billing: billing.clone(),\n        });\n' + anchor)
            anchor = '                || path == "/client/negotiate"'
            if registry.count(anchor) != 1:
                raise RuntimeError('Public route anchor mismatch')
            registry = registry.replace(anchor, anchor + '\n                || path == "/api/v1/announcements"')
            rc.write_remote(ssh, dest + '/' + REGISTRY, registry.encode())
            rc.write_remote(ssh, dest + '/' + TEST, (rc.ROOT / TEST).read_bytes())
            # Overlay only audited public assets; preserve old hashed assets for open tabs.
            web_files = [rc.ROOT / 'apps/portal-ui/index.html'] + sorted(
                p for p in (rc.ROOT / 'apps/admin-ui/dist').rglob('*') if p.is_file())
            if not (rc.ROOT / 'apps/admin-ui/dist/index.html').is_file():
                raise RuntimeError('Admin build missing')
            report['web_sha256'] = {}
            for local in web_files:
                url = '/' if local.name == 'index.html' and 'portal-ui' in local.parts else '/admin/' + local.relative_to(rc.ROOT / 'apps/admin-ui/dist').as_posix()
                if url == '/admin/index.html':
                    url = '/admin/'
                report['web_sha256'][url] = hashlib.sha256(local.read_bytes()).hexdigest()
            with ssh.open_sftp() as sftp:
                for local in web_files:
                    relative = local.relative_to(rc.ROOT).as_posix()
                    content = local.read_bytes()
                    try:
                        with sftp.open(dest + '/' + relative, 'rb') as remote:
                            unchanged = remote.read() == content
                    except FileNotFoundError:
                        unchanged = False
                    if unchanged:
                        continue
                    rc.run(ssh, 'mkdir -p ' + shlex.quote(str(Path(dest + '/' + relative).parent).replace('\\', '/')))
                    rc.write_remote(ssh, dest + '/' + relative, content)
                    sftp.chmod(dest + '/' + relative, 0o644)
                    report['scope'].append(relative)
            compose = read(ssh, dest + '/deploy/docker-compose.ip.yml')
            images = re.findall(r'(?m)^    image: (kiro-byok:[^\s]+)$', compose)
            if len(images) != 1:
                raise RuntimeError('Unexpected compose image')
            report['previous_image'] = images[0]
            rc.write_remote(ssh, dest + '/deploy/docker-compose.ip.yml', compose.replace('    image: ' + images[0], '    image: ' + report['image'], 1).encode())
            # Compare every copied input against the explicit endpoint + web allowlist.
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
assert set(a).issubset(b)
print(json.dumps(sorted(k for k in b if a.get(k)!=b[k])))
'''
            changed = json.loads(rc.run(ssh, 'python3 -c ' + shlex.quote(diff_script) + ' ' + shlex.join([old, dest, json.dumps(paths)])))
            if changed != sorted(report['scope']):
                raise RuntimeError('Candidate changes exceed allowlist')
            report['verified_changed_paths'] = changed
            diff = ''.join(difflib.unified_diff(original.splitlines(True), patched.splitlines(True), fromfile='live/' + CLIENT, tofile='candidate/' + CLIENT))
            (rc.ROOT / 'deploy/announcements-live.patch').write_text(diff, encoding='utf8')
            report['candidate_sha256'] = rc.tree_digest(ssh, dest, paths)
            rc.save_report(ssh, report)
            print('STAGED ' + release + ': exact allowlist verified', flush=True)
            # Reuse the exact preceding builder's dependency artifacts, not an unverified binary.
            base = rc.run(ssh, "docker image inspect kiro-device-test:20260919T130652Z --format '{{.Id}}'")
            if not re.fullmatch(r'sha256:[a-f0-9]{64}', base):
                raise RuntimeError('Previous builder unavailable')
            rc.run(ssh, f'docker run --rm -v {old}:/baseline:ro {base} sh -c "diff -qr /build/crates /baseline/crates && cmp /build/Cargo.lock /baseline/Cargo.lock && cmp /build/Cargo.toml /baseline/Cargo.toml"')
            report['verified_builder_base'] = base
            dockerfile = read(ssh, dest + '/deploy/Dockerfile').replace('\r', '')
            runtime = dockerfile[dockerfile.index('FROM debian:'):]
            build = f'FROM {base} AS builder\nWORKDIR /build\n'
            for path in [CLIENT, REGISTRY, TEST]:
                build += f'COPY {path} {path}\n'
            build += 'COPY apps/portal-ui/index.html apps/portal-ui/index.html\n'
            build += 'RUN CARGO_BUILD_JOBS=1 cargo test --locked -p gateway --test announcements_test && CARGO_BUILD_JOBS=1 cargo build --locked --release --package gateway --bin gateway\n'
            rc.write_remote(ssh, dest + '/Dockerfile.announcements', (build + runtime).encode())
            script = f"""set -eu
cd {dest}
if docker build --iidfile build.image-id -t {report['image']} -f Dockerfile.announcements . >build.log 2>&1; then code=0; else code=$?; fi
printf '%s' "$code" >build.exit
exit "$code"
"""
            rc.write_remote(ssh, dest + '/limited-build.sh', script.encode())
            rc.run(ssh, 'sh ' + dest + '/limited-build.sh', timeout=7200)
            report['announcement_tests'] = rc.run(ssh, f"grep 'test result:' {dest}/build.log")
            rc.save_report(ssh, report)
            print('PASS server tests and release build', flush=True)
            if '--stage-only' in sys.argv:
                print('READY ' + release + ': production unchanged', flush=True)
                return
            rc.promote(ssh, report, extra_readiness=verify_public_release)
            rc.wait_gateway(ssh, report['image_id'])
            rc.external_readiness()
            report['final_current'] = rc.run(ssh, 'readlink -f /opt/kiro-byok/current')
            if report['final_current'] != dest:
                raise RuntimeError('Final release mismatch')
            report['verification'] = 'passed'
            rc.save_report(ssh, report)
            (rc.ROOT / 'deploy/announcements-release-results.json').write_text(json.dumps(report, indent=2), encoding='utf8')
            print('COMPLETE ' + release, flush=True)
    finally:
        ssh.close()


if __name__ == '__main__':
    main()
