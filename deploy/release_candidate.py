"""Stage and promote pinned candidates; never print remote output or credentials."""
import hashlib
import json
from contextlib import contextmanager
from pathlib import Path
import re
import shlex
import sys
import tarfile
import time
from datetime import datetime, timezone

import requests

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from test_deployed_server import ROOT, connect

BASE = '/opt/kiro-byok'
SOURCE_PATHS = ['Cargo.toml', 'Cargo.lock', 'crates', 'apps/admin-ui/dist', 'apps/portal-ui', 'deploy/Dockerfile']


def run(ssh, command, timeout=300):
    # A failed command or pipeline must not be hidden by a later successful command.
    command = 'bash -o pipefail -c ' + shlex.quote('set -eu\numask 077\n' + command) + ' 2>&1'
    try:
        _, out, _ = ssh.exec_command(command, timeout=timeout)
        text = out.read().decode('utf-8', errors='replace')
        code = out.channel.recv_exit_status()
    except Exception:
        raise RuntimeError('Remote operation interrupted; inspect server state before retrying') from None
    if code:
        raise RuntimeError(f'Remote operation failed (exit {code}); remote output withheld')
    return text.strip()


def pinned_connection(credentials):
    # The existing helper uses assert for host-key verification. Do not call it under -O.
    if not __debug__:
        raise RuntimeError('Optimized Python is forbidden for SSH host-key verification')
    try:
        return connect(credentials['password'], use_proxy=credentials.get('use_proxy', False))
    except Exception:
        raise RuntimeError('Pinned SSH connection failed') from None


@contextmanager
def deployment_lock(ssh):
    lock = BASE + '/deployment.lock'
    run(ssh, f'mkdir -m 700 {lock}')
    try:
        yield
    except Exception:
        # A timed-out SSH command may still be running. Never unlock on uncertainty.
        raise RuntimeError('Deployment failed; lock retained at /opt/kiro-byok/deployment.lock for operator review') from None
    else:
        run(ssh, f'rmdir {lock}')


def release_path(value):
    if not isinstance(value, str) or not re.fullmatch(r'/opt/kiro-byok/releases/[A-Za-z0-9_-]+', value):
        raise ValueError('Unexpected release path')
    return value


def tree_digest(ssh, directory, paths):
    text = run(ssh, f'cd {shlex.quote(directory)}\nfind {shlex.join(paths)} -type f -print0 | LC_ALL=C sort -z | xargs -0 -r sha256sum')
    if not text:
        raise RuntimeError('Empty release inputs')
    return hashlib.sha256(text.encode()).hexdigest()


def configuration_digest(ssh, release):
    deployment = tree_digest(ssh, release + '/deploy', ['docker-compose.ip.yml', 'Caddyfile.ip'])
    protected = tree_digest(ssh, '/etc/kiro-byok', ['.'])
    return hashlib.sha256((deployment + protected).encode()).hexdigest()


def write_remote(ssh, path, content):
    with ssh.open_sftp() as sftp:
        with sftp.open(path + '.next', 'wb') as file:
            file.write(content)
        sftp.chmod(path + '.next', 0o600)
        sftp.posix_rename(path + '.next', path)


def save_report(ssh, report):
    content = json.dumps(report, indent=2).encode()
    path = ROOT / 'deployment-candidate-results.json'
    temporary = path.with_suffix('.next')
    temporary.write_bytes(content)
    temporary.replace(path)
    write_remote(ssh, f"{BASE}/releases/{report['release']}/release-state.json", content)


def admin_readiness(admin, api, browser_login=True):
    if api.status_code != 401:
        return False
    if not browser_login:
        return admin.status_code == 401
    return (admin.status_code == 200
            and 'text/html' in admin.headers.get('Content-Type', '')
            and 'WWW-Authenticate' not in admin.headers
            and 'WWW-Authenticate' not in api.headers)


def external_readiness(browser_login=True):
    # No credentials, proxy inheritance, redirects, or private CA overrides.
    with requests.Session() as http:
        http.trust_env = False
        for attempt in range(30):
            try:
                health = http.get('https://kiro.rent/healthz', timeout=8, allow_redirects=False)
                admin = http.get('https://kiro.rent/admin/', timeout=8, allow_redirects=False)
                api = http.get('https://kiro.rent/api/v1/admin/stats', timeout=8, allow_redirects=False)
                post = http.post('https://kiro.rent/', json={}, timeout=8, allow_redirects=False)
                if (health.status_code == 200 and admin_readiness(admin, api, browser_login)
                        and 200 <= post.status_code < 500
                        and post.headers.get('Content-Type', '').split(';', 1)[0] in ('application/json', 'application/x-amz-json-1.1', 'application/x-amz-json-1.0')):
                    post.json()
                    return
            except (requests.RequestException, ValueError):
                pass
            if attempt < 29:
                time.sleep(2)
    raise RuntimeError('Public TLS readiness or unauthenticated access checks failed')


def wait_gateway(ssh, image_id):
    run(ssh, "for i in $(seq 1 60); do "
        "[ \"$(docker inspect kiro-gateway --format '{{.State.Health.Status}}')\" = healthy ] && break; "
        "sleep 2; done\n"
        "test \"$(docker inspect kiro-gateway --format '{{.State.Health.Status}}')\" = healthy")
    if run(ssh, "docker inspect kiro-gateway --format '{{.Image}}'") != image_id:
        raise RuntimeError('Running gateway does not match pinned image')


def stop_services(ssh):
    run(ssh, 'docker stop -t 30 kiro-caddy\ndocker stop -t 90 kiro-gateway')
    for name in ('kiro-caddy', 'kiro-gateway'):
        if run(ssh, f"docker inspect {name} --format '{{{{.State.Running}}}}'") != 'false':
            raise RuntimeError('Services are not stopped')


def switch_current(ssh, target, release):
    link = BASE + '/current.next-' + release
    run(ssh, f'ln -s {target} {link}\nmv -Tf {link} {BASE}/current')


def promote(ssh, report, extra_readiness=None):
    """Caller holds deployment_lock; both build entry points use this transaction."""
    release = report['release']
    if not re.fullmatch(r'\d{8}T\d{6}Z', release) or report.get('status') != 'staging':
        raise ValueError('Candidate is not staged')
    dest = release_path(f'{BASE}/releases/{release}')
    old = release_path(report['previous_release'])
    backup = f'{BASE}/backups/release-{release}'
    compose = f'docker compose -p deploy -f {dest}/deploy/docker-compose.ip.yml'
    previous = f'docker compose -p deploy -f {old}/deploy/docker-compose.ip.yml'
    if run(ssh, f'cat {dest}/build.exit') != '0':
        raise RuntimeError('Candidate build did not succeed')
    if run(ssh, f'readlink -f {BASE}/current') != old:
        raise RuntimeError('Current release changed; restage candidate')
    if configuration_digest(ssh, old) != report['configuration_sha256']:
        raise RuntimeError('Production configuration changed; restage candidate')
    if tree_digest(ssh, dest, SOURCE_PATHS + ['deploy/docker-compose.ip.yml', 'deploy/Caddyfile.ip']) != report['candidate_sha256']:
        raise RuntimeError('Staged candidate changed; restage candidate')
    if report.get('browser_auth_migration'):
        from deploy.migrate_browser_auth import verify_staged
        verify_staged(ssh, report)
    image_id = run(ssh, f'cat {dest}/build.image-id')
    if not re.fullmatch(r'sha256:[a-f0-9]{64}', image_id):
        raise RuntimeError('Missing build image identity')
    if run(ssh, f"docker image inspect kiro-byok:{release} --format '{{{{.Id}}}}'") != image_id:
        raise RuntimeError('Candidate image tag changed')
    config = json.loads(run(ssh, compose + ' config --format json'))
    if config['services']['gateway']['image'] != f'kiro-byok:{release}':
        raise RuntimeError('Compose gateway image differs from candidate')
    previous_id = report['previous_image_id']
    if run(ssh, "docker inspect kiro-gateway --format '{{.Image}}'") != previous_id:
        raise RuntimeError('Previous gateway identity is unavailable')
    old_config = json.loads(run(ssh, previous + ' config --format json'))
    old_tag = shlex.quote(old_config['services']['gateway']['image'])
    if run(ssh, f"docker image inspect {old_tag} --format '{{{{.Id}}}}'") != previous_id:
        raise RuntimeError('Rollback image tag changed')
    report['image_id'] = image_id
    run(ssh, f'mkdir -m 700 {backup}\ncp -a {old}/deploy {backup}/deploy\ncp -a /etc/kiro-byok {backup}/configuration\n'
        f'diff -qr {old}/deploy {backup}/deploy\ndiff -qr /etc/kiro-byok {backup}/configuration')
    report['status'] = 'stopping'
    save_report(ssh, report)
    try:
        stop_services(ssh)
        exit_code = run(ssh, "docker inspect kiro-gateway --format '{{.State.ExitCode}}'")
        oom_killed = run(ssh, "docker inspect kiro-gateway --format '{{.State.OOMKilled}}'")
        # Current gateway handles SIGTERM and exits 0; older images may exit 143.
        # Neither code proves a flushed snapshot. Preserve the stopped data verbatim.
        if exit_code not in ('0', '143') or oom_killed != 'false':
            raise RuntimeError('Gateway stop was abnormal; data needs review')
        report['previous_exit_code'] = int(exit_code)
        run(ssh, f'cp -a {BASE}/data {backup}/data\ndiff -qr {BASE}/data {backup}/data\ntouch {backup}/data.complete')
        report['status'] = 'backed_up'
        save_report(ssh, report)
        if report.get('browser_auth_migration'):
            from deploy.migrate_browser_auth import apply_staged, rehearse
            rehearse(ssh, report)
            apply_staged(ssh, report)
        run(ssh, compose + ' up -d --no-deps --pull never gateway')
        wait_gateway(ssh, image_id)
        switch_current(ssh, dest, release)
        # Persist before invoking Caddy: the command may succeed remotely and time out locally.
        report['status'] = 'exposing'
        save_report(ssh, report)
        run(ssh, compose + ' up -d --no-deps --pull never --force-recreate caddy')
        external_readiness()
        wait_gateway(ssh, image_id)
        if run(ssh, "docker inspect kiro-caddy --format '{{.State.Running}}'") != 'true':
            raise RuntimeError('Ingress stopped during readiness')
        if extra_readiness is not None:
            extra_readiness(report)
        report['status'] = 'deployed'
        save_report(ssh, report)
    except Exception:
        phase = report['status']
        report['status'] = 'needs_attention'
        try:
            if phase == 'backed_up':
                stop_services(ssh)
                # Copy/verify first. Never overwrite either the backup or failed candidate data.
                restore = f'{BASE}/data.restore-{release}'
                run(ssh, f'test -f {backup}/data.complete\ntest ! -e {restore}\n'
                    f'cp -a {backup}/data {restore}\ndiff -qr {backup}/data {restore}\n'
                    f'test ! -e {backup}/failed-candidate-data\nmv {BASE}/data {backup}/failed-candidate-data\nmv {restore} {BASE}/data')
                if report.get('browser_auth_migration'):
                    run(ssh, f'cp -a {backup}/configuration/gateway.env /etc/kiro-byok/gateway.env\ncp -a {backup}/configuration/caddy.env /etc/kiro-byok/caddy.env')
                switch_current(ssh, old, release)
                run(ssh, previous + ' up -d --no-deps --pull never --force-recreate gateway')
                wait_gateway(ssh, previous_id)
                run(ssh, previous + ' up -d --no-deps --pull never --force-recreate caddy')
                old_browser_login = str(old_config['services']['gateway'].get('environment', {}).get('ADMIN_BROWSER_LOGIN', '')).lower() == 'true'
                external_readiness(browser_login=old_browser_login)
                report['status'] = 'rolled_back'
            else:
                # No data rewind after exposure (including an uncertain SSH outcome).
                stop_services(ssh)
        except Exception:
            report['status'] = 'rollback_failed' if phase == 'backed_up' else 'needs_attention'
        save_report(ssh, report)
        raise RuntimeError('Candidate promotion failed; inspect persisted release state') from None


def main(credentials=None, ssh=None):
    credentials = json.load(sys.stdin) if credentials is None else credentials
    owns_connection = ssh is None
    if owns_connection:
        ssh = pinned_connection(credentials)
    release = datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ')
    dest = f'{BASE}/releases/{release}'
    report = {'release': release, 'image': f'kiro-byok:{release}',
              'backup': f'{BASE}/backups/release-{release}', 'status': 'staging'}
    try:
        with deployment_lock(ssh):
            old = release_path(run(ssh, f'readlink -f {BASE}/current'))
            report['previous_release'] = old
            report['configuration_sha256'] = configuration_digest(ssh, old)
            report['previous_image_id'] = run(ssh, "docker inspect kiro-gateway --format '{{.Image}}'")
            with ssh.open_sftp() as sftp:
                compose = sftp.open(old + '/deploy/docker-compose.ip.yml').read().decode()
                matches = re.findall(r'(?m)^    image: (kiro-byok:[^\s]+)$', compose)
                if len(matches) != 1:
                    raise RuntimeError('Unexpected gateway image configuration')
                report['previous_image'] = matches[0]
                compose = compose.replace('    image: ' + matches[0], '    image: ' + report['image'], 1)
                caddy = sftp.open(old + '/deploy/Caddyfile.ip').read()
                archive = ROOT / '.acceptance' / f'release-{release}.tar.gz'
                archive.parent.mkdir(parents=True, exist_ok=True)
                for relative in SOURCE_PATHS + ['apps/admin-ui/dist/index.html', 'apps/portal-ui/index.html']:
                    if not (ROOT / relative).exists():
                        raise RuntimeError('Required release source or frontend asset missing')
                with tarfile.open(archive, 'w:gz') as tar:
                    for relative in SOURCE_PATHS:
                        path = ROOT / relative
                        files = [path] if path.is_file() else sorted(p for p in path.rglob('*') if p.is_file() and not {'target', 'node_modules', '__pycache__', '.git', 'test-artifacts'}.intersection(p.relative_to(ROOT).parts))
                        for file in files:
                            tar.add(file, arcname=file.relative_to(ROOT).as_posix(), recursive=False)
                digest = hashlib.sha256(archive.read_bytes()).hexdigest()
                report['archive_sha256'] = digest
                run(ssh, f'mkdir {dest}\nmkdir {dest}/deploy')
                sftp.put(str(archive), dest + '/source.tar.gz')
            write_remote(ssh, dest + '/deploy/docker-compose.ip.yml', compose.encode())
            write_remote(ssh, dest + '/deploy/Caddyfile.ip', caddy)
            run(ssh, f"echo '{digest}  {dest}/source.tar.gz' | sha256sum -c -\ntar -xzf {dest}/source.tar.gz -C {dest}")
            report['candidate_sha256'] = tree_digest(ssh, dest, SOURCE_PATHS + ['deploy/docker-compose.ip.yml', 'deploy/Caddyfile.ip'])
            script = (f'set -eu\numask 077\ncd {dest}\n'
                      f'if docker build --iidfile build.image-id -t {report["image"]} -f deploy/Dockerfile . > build.log 2>&1; '
                      'then code=0; else code=$?; fi\nprintf "%s" "$code" > build.exit.next\nmv build.exit.next build.exit\nexit "$code"\n')
            write_remote(ssh, dest + '/build-resume.sh', script.encode())
            save_report(ssh, report)
            if credentials.get('stage_only', False):
                run(ssh, f'nohup sh {dest}/build-resume.sh >/dev/null 2>&1 < /dev/null &')
            else:
                run(ssh, f'sh {dest}/build-resume.sh', timeout=3600)
                promote(ssh, report)
    finally:
        if owns_connection:
            ssh.close()


if __name__ == '__main__':
    main()
