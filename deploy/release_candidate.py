"""Stage and promote pinned candidates; never print remote output or credentials."""
import hashlib
import json
from contextlib import contextmanager
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import tarfile
import time
from datetime import datetime, timezone

import requests

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from test_deployed_server import ROOT, connect

BASE = '/opt/kiro-byok'
SOURCE_PATHS = ['Cargo.toml', 'Cargo.lock', 'crates', 'apps/admin-ui/dist', 'apps/portal-ui', 'deploy/Dockerfile']
# What the shipped sources are built from; all of it must be exactly the release commit.
COMMITTED_INPUTS = ['Cargo.toml', 'Cargo.lock', 'crates', 'apps/portal-ui', 'deploy/Dockerfile',
                    'apps/admin-ui/src', 'apps/admin-ui/index.html', 'apps/admin-ui/package.json',
                    'apps/admin-ui/package-lock.json', 'apps/admin-ui/vite.config.ts',
                    'apps/admin-ui/tsconfig.json', 'apps/admin-ui/tailwind.config.js',
                    'apps/admin-ui/postcss.config.js']
LEDGER_ANCHOR = BASE + '/data/billing_state.json.anchor'
# Releases, with their data and configuration copies, kept after a deploy.
KEEP_RELEASES = 5
FREE_SPACE_MARGIN = 1 << 30
RELEASE_NAME = re.compile(r'\d{8}T\d{6}Z')


class PreconditionFailed(RuntimeError):
    """Refused before anything changed: the lock is released and the reason kept."""


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
    except PreconditionFailed:
        # Nothing was changed, so nothing needs review: release the lock, keep the reason.
        run(ssh, f'rmdir {lock}')
        raise
    except Exception as error:
        # A timed-out SSH command may still be running. Never unlock on uncertainty.
        # The reason is our own message or an exception type; remote output is never in it.
        raise RuntimeError(f'Deployment failed ({type(error).__name__}: {error}); lock retained at '
                           '/opt/kiro-byok/deployment.lock for operator review') from None
    else:
        run(ssh, f'rmdir {lock}')


def git(*args, root=ROOT):
    return subprocess.run(['git', '-C', str(root), *args], capture_output=True, text=True,
                          check=True).stdout.strip()


def ci_conclusions(commit, root=ROOT):
    """Every CI check run on `commit`, as its conclusion, or 'pending' while it runs."""
    try:
        origin = git('remote', 'get-url', 'origin', root=root)
        repo = re.search(r'github\.com[:/]([^/]+/[^/]+?)(?:\.git)?$', origin).group(1)
        out = subprocess.run(
            [shutil.which('gh') or 'gh', 'api', '--paginate', f'repos/{repo}/commits/{commit}/check-runs',
             '-q', '.check_runs[] | .status + " " + (.conclusion // "")'],
            capture_output=True, text=True, check=True).stdout
    except (OSError, AttributeError, subprocess.CalledProcessError):
        raise PreconditionFailed('Cannot read CI results for the release commit') from None
    runs = [line.split() for line in out.splitlines() if line.strip()]
    return [fields[1] if fields[0] == 'completed' and len(fields) > 1 else 'pending' for fields in runs]


def verified_commit(allow_untested=False, root=ROOT):
    """The commit a release is built from.

    CI is the only place the full test suite runs, so a release is built only from a commit
    CI passed: on origin/main, with every committed input exactly that commit, nothing
    edited or added on this disk. `allow_untested` skips the CI result only, for an
    emergency, and the report still records the commit.
    """
    try:
        commit = git('rev-parse', 'HEAD', root=root)
        dirty = git('status', '--porcelain', '--untracked-files=all', '--', *COMMITTED_INPUTS, root=root)
        git('fetch', '--quiet', 'origin', 'main', root=root)
        on_main = subprocess.run(['git', '-C', str(root), 'merge-base', '--is-ancestor', commit, 'origin/main'],
                                 capture_output=True).returncode == 0
    except (OSError, subprocess.CalledProcessError):
        raise PreconditionFailed('Cannot read the release source from git') from None
    if dirty:
        raise PreconditionFailed('Release inputs have uncommitted or untracked changes')
    if not on_main:
        raise PreconditionFailed('Release commit is not on origin/main')
    if not allow_untested:
        conclusions = ci_conclusions(commit, root)
        if not conclusions or any(c not in ('success', 'skipped', 'neutral') for c in conclusions):
            raise PreconditionFailed('CI has not passed for the release commit')
    return commit


def build_admin_ui(root=ROOT):
    """Build the admin bundle from the release commit's own sources and lockfile."""
    npm = shutil.which('npm')
    if npm is None:
        raise PreconditionFailed('npm is required to build the admin UI')
    for args in (['ci', '--no-audit', '--no-fund'], ['run', 'build']):
        if subprocess.run([npm, *args], cwd=root / 'apps' / 'admin-ui', capture_output=True).returncode:
            raise PreconditionFailed('Admin UI build failed')


def ledger_sequence(ssh):
    """The saved ledger's sequence, from its anchor; None for a machine with no ledger."""
    text = run(ssh, f"if test -f {LEDGER_ANCHOR}; then grep -o '\"sequence\": *[0-9]*' {LEDGER_ANCHOR} "
                    "| grep -o '[0-9]*$' | head -n 1; fi")
    return int(text) if text.isdigit() else None


def verify_ledger_loaded(ssh, sequence):
    """The new gateway must say it restored exactly the ledger it was given. A missing or
    empty data directory otherwise starts a healthy gateway that knows no card."""
    if sequence is None:
        return
    restored = run(ssh, "docker logs kiro-gateway 2>&1 | grep -o 'Restored billing state at sequence [0-9]*' "
                        "| tail -n 1")
    if restored != f'Restored billing state at sequence {sequence}':
        raise RuntimeError('New gateway did not load the ledger it was given')


def check_free_space(ssh):
    """Room for the data copy, before anything is stopped."""
    free, used = (int(value) for value in run(
        ssh, f"df --output=avail -B1 {BASE} | tail -n 1\ndu -sb {BASE}/data | cut -f1").split())
    if free < 2 * used + FREE_SPACE_MARGIN:
        raise PreconditionFailed('Not enough free space for the data backup; nothing was stopped')


def pull_tree(ssh, remote, local):
    """Copy a remote directory here, checking every file's SHA-256 against the server's."""
    local = Path(local)
    listing = run(ssh, f"cd {shlex.quote(remote)}\nfind . -type f -print0 | LC_ALL=C sort -z | xargs -0 -r sha256sum")
    with ssh.open_sftp() as sftp:
        for line in listing.splitlines():
            digest, relative = line.split('  ', 1)
            relative = relative[2:] if relative.startswith('./') else relative
            if relative.startswith('/') or '..' in Path(relative).parts:
                raise RuntimeError('Unexpected backup path')
            target = local / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            sftp.get(f'{remote}/{relative}', str(target))
            if hashlib.sha256(target.read_bytes()).hexdigest() != digest:
                raise RuntimeError('Backup copy does not match the server')
    return str(local)


def prune_releases(ssh, protect):
    """Keep the newest KEEP_RELEASES releases, their data and configuration copies and
    images, and every release in `protect`. Remove the rest. Only timestamp-named entries
    are ever touched."""
    names = sorted(name for name in run(ssh, f'ls -1 {BASE}/releases').split() if RELEASE_NAME.fullmatch(name))
    doomed = [name for name in names[:-KEEP_RELEASES] if f'{BASE}/releases/{name}' not in protect]
    for name in doomed:
        run(ssh, f'rm -rf -- {BASE}/releases/{name} {BASE}/backups/release-{name} {BASE}/config-backups/release-{name}\n'
                 f'docker image rm kiro-byok:{name} >/dev/null 2>&1 || true')
    return doomed


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


def portal_caddy_config(content):
    """Preserve live routing/secrets; add only the embedded-font CSP permission."""
    text = content.decode('utf-8')
    pattern = r'Content-Security-Policy "([^"\n]+)"'
    matches = list(re.finditer(pattern, text))
    if len(matches) != 1:
        raise RuntimeError('Expected exactly one production CSP header')
    match = matches[0]
    policy = match.group(1)
    fonts = [part.strip() for part in policy.split(';') if part.strip().startswith('font-src')]
    if fonts:
        if fonts != ["font-src 'self' data:"]:
            raise RuntimeError('Custom font policy requires explicit review')
        return content
    policy = policy.rstrip('; ') + "; font-src 'self' data:"
    return (text[:match.start(1)] + policy + text[match.end(1):]).encode('utf-8')


def save_report(ssh, report):
    content = json.dumps(report, indent=2).encode()
    write_remote(ssh, f"{BASE}/releases/{report['release']}/release-state.json", content)
    # The server's copy is the record. This local one is a convenience, and a scanner or
    # editor holding the file must never fail a production step.
    path = ROOT / 'deployment-candidate-results.json'
    for _ in range(5):
        try:
            temporary = path.with_suffix('.next')
            temporary.write_bytes(content)
            temporary.replace(path)
            return
        except OSError:
            time.sleep(0.5)


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
    # Secrets are backed up apart from the ledger, so no backup directory holds both the
    # ciphertext and its key.
    configuration = f'{BASE}/config-backups/release-{release}'
    compose = f'docker compose -p deploy -f {dest}/deploy/docker-compose.ip.yml'
    previous = f'docker compose -p deploy -f {old}/deploy/docker-compose.ip.yml'
    if run(ssh, f'cat {dest}/build.exit') != '0':
        raise PreconditionFailed('Candidate build did not succeed')
    if run(ssh, f'readlink -f {BASE}/current') != old:
        raise PreconditionFailed('Current release changed; restage candidate')
    if configuration_digest(ssh, old) != report['configuration_sha256']:
        raise PreconditionFailed('Production configuration changed; restage candidate')
    if tree_digest(ssh, dest, SOURCE_PATHS + ['deploy/docker-compose.ip.yml', 'deploy/Caddyfile.ip']) != report['candidate_sha256']:
        raise PreconditionFailed('Staged candidate changed; restage candidate')
    if report.get('browser_auth_migration'):
        from deploy.migrate_browser_auth import verify_staged
        verify_staged(ssh, report)
    image_id = run(ssh, f'cat {dest}/build.image-id')
    if not re.fullmatch(r'sha256:[a-f0-9]{64}', image_id):
        raise PreconditionFailed('Missing build image identity')
    if run(ssh, f"docker image inspect kiro-byok:{release} --format '{{{{.Id}}}}'") != image_id:
        raise PreconditionFailed('Candidate image tag changed')
    config = json.loads(run(ssh, compose + ' config --format json'))
    if config['services']['gateway']['image'] != f'kiro-byok:{release}':
        raise PreconditionFailed('Compose gateway image differs from candidate')
    previous_id = report['previous_image_id']
    if run(ssh, "docker inspect kiro-gateway --format '{{.Image}}'") != previous_id:
        raise PreconditionFailed('Previous gateway identity is unavailable')
    old_config = json.loads(run(ssh, previous + ' config --format json'))
    old_tag = shlex.quote(old_config['services']['gateway']['image'])
    if run(ssh, f"docker image inspect {old_tag} --format '{{{{.Id}}}}'") != previous_id:
        raise PreconditionFailed('Rollback image tag changed')
    report['image_id'] = image_id
    check_free_space(ssh)
    run(ssh, f'mkdir -m 700 {backup}\ncp -a {old}/deploy {backup}/deploy\n'
        f'mkdir -p -m 700 {BASE}/config-backups\ncp -a /etc/kiro-byok {configuration}\n'
        f'diff -qr {old}/deploy {backup}/deploy\ndiff -qr /etc/kiro-byok {configuration}')
    report['status'] = 'stopping'
    save_report(ssh, report)
    clean_stop = False
    try:
        stop_services(ssh)
        exit_code = run(ssh, "docker inspect kiro-gateway --format '{{.State.ExitCode}}'")
        oom_killed = run(ssh, "docker inspect kiro-gateway --format '{{.State.OOMKilled}}'")
        # Current gateway handles SIGTERM and exits 0; older images may exit 143.
        # Neither code proves a flushed snapshot. Preserve the stopped data verbatim.
        if exit_code not in ('0', '143') or oom_killed != 'false':
            raise RuntimeError('Gateway stop was abnormal; data needs review')
        report['previous_exit_code'] = int(exit_code)
        clean_stop = True
        run(ssh, f'cp -a {BASE}/data {backup}/data\ndiff -qr {BASE}/data {backup}/data\ntouch {backup}/data.complete')
        # Read once the old gateway has flushed and stopped: what the new one must load.
        report['ledger_sequence'] = ledger_sequence(ssh)
        report['status'] = 'backed_up'
        save_report(ssh, report)
        if report.get('browser_auth_migration'):
            from deploy.migrate_browser_auth import apply_staged, rehearse
            rehearse(ssh, report)
            apply_staged(ssh, report)
        run(ssh, compose + ' up -d --no-deps --pull never gateway')
        wait_gateway(ssh, image_id)
        verify_ledger_loaded(ssh, report['ledger_sequence'])
        switch_current(ssh, dest, release)
        # Persist before invoking Caddy: the command may succeed remotely and time out locally.
        report['status'] = 'exposing'
        save_report(ssh, report)
        run(ssh, compose + ' up -d --no-deps --pull never --force-recreate caddy')
        external_readiness()
        wait_gateway(ssh, image_id)
        if run(ssh, "docker inspect kiro-caddy --format '{{.State.Running}}'") != 'true':
            raise RuntimeError('Ingress stopped during readiness')
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
                    run(ssh, f'cp -a {configuration}/gateway.env /etc/kiro-byok/gateway.env\ncp -a {configuration}/caddy.env /etc/kiro-byok/caddy.env')
                switch_current(ssh, old, release)
                run(ssh, previous + ' up -d --no-deps --pull never --force-recreate gateway')
                wait_gateway(ssh, previous_id)
                run(ssh, previous + ' up -d --no-deps --pull never --force-recreate caddy')
                old_browser_login = str(old_config['services']['gateway'].get('environment', {}).get('ADMIN_BROWSER_LOGIN', '')).lower() == 'true'
                external_readiness(browser_login=old_browser_login)
                report['status'] = 'rolled_back'
            elif phase == 'exposing':
                # The new gateway is healthy and current. If ingress never came back up
                # (the save or the Caddy start itself failed), bring it up rather than
                # leave the site down. A running Caddy means readiness failed: leave the
                # live release to the operator, as below.
                if run(ssh, "docker inspect kiro-caddy --format '{{.State.Running}}'") != 'true':
                    run(ssh, compose + ' up -d --no-deps --pull never caddy')
                    external_readiness()
            elif phase == 'stopping' and clean_stop:
                # The old gateway stopped cleanly and nothing was changed: only the copy
                # failed (no space, an I/O error, a dropped command). Bring the previous
                # release back rather than leave the site down.
                run(ssh, f'test ! -e {backup}/data.complete\nrm -rf -- {backup}/data')
                run(ssh, previous + ' up -d --no-deps --pull never gateway')
                wait_gateway(ssh, previous_id)
                run(ssh, previous + ' up -d --no-deps --pull never caddy')
                old_browser_login = str(old_config['services']['gateway'].get('environment', {}).get('ADMIN_BROWSER_LOGIN', '')).lower() == 'true'
                external_readiness(browser_login=old_browser_login)
                report['status'] = 'restarted_previous'
            elif phase == 'stopping':
                # The stop itself was abnormal: the data may need review, so leave the
                # services down for an operator.
                stop_services(ssh)
            # Every other phase leaves production running. At 'staging' nothing has
            # touched it yet, so the previous release is still serving normally. At
            # 'exposing' the new release is already healthy, serving, and writing to
            # the data directory: its data cannot be rewound, but that is a reason to
            # hand the decision to an operator, not to stop the site. A live but
            # unverified release beats an outage. 'needs_attention' is set above and
            # the lock is retained, so the next step is deliberate either way.
        except Exception:
            report['status'] = 'rollback_failed' if phase == 'backed_up' else 'needs_attention'
        save_report(ssh, report)
        raise RuntimeError('Candidate promotion failed; inspect persisted release state') from None
    # Verification, not promotion. The release is already live and recorded as
    # deployed, so a public asset check failing here reports a problem instead of
    # tearing down ingress.
    if extra_readiness is not None:
        extra_readiness(report)


def main(credentials=None, ssh=None):
    credentials = json.load(sys.stdin) if credentials is None else credentials
    commit = verified_commit(allow_untested=credentials.get('allow_untested', False))
    build_admin_ui()
    owns_connection = ssh is None
    if owns_connection:
        ssh = pinned_connection(credentials)
    release = datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ')
    dest = f'{BASE}/releases/{release}'
    report = {'release': release, 'image': f'kiro-byok:{release}', 'commit': commit,
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
                caddy = portal_caddy_config(sftp.open(old + '/deploy/Caddyfile.ip').read())
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
                      f'if docker build --iidfile build.image-id --label org.opencontainers.image.revision={commit} '
                      f'-t {report["image"]} -f deploy/Dockerfile . > build.log 2>&1; '
                      'then code=0; else code=$?; fi\nprintf "%s" "$code" > build.exit.next\nmv build.exit.next build.exit\nexit "$code"\n')
            write_remote(ssh, dest + '/build-resume.sh', script.encode())
            save_report(ssh, report)
            if credentials.get('stage_only', False):
                run(ssh, f'nohup sh {dest}/build-resume.sh >/dev/null 2>&1 < /dev/null &')
            else:
                run(ssh, f'sh {dest}/build-resume.sh', timeout=3600)
                promote(ssh, report)
                keep_off_host(ssh, report, credentials)
    finally:
        if owns_connection:
            ssh.close()


def keep_off_host(ssh, report, credentials):
    """After a deploy: copy the release-time ledger backup here, off the production disk,
    and prune old releases. The release is live either way; failures are reported, not
    raised. The copy holds the encrypted ledger only; the KEK stays on the server."""
    local = Path(credentials.get('backup_dir') or Path.home() / 'kiro-byok-backups') / report['release']
    try:
        report['off_host_backup'] = pull_tree(ssh, report['backup'] + '/data', local / 'data')
    except Exception as error:
        report['off_host_backup'] = f'failed ({type(error).__name__}: {error})'
    try:
        report['pruned_releases'] = prune_releases(
            ssh, {f"{BASE}/releases/{report['release']}", report['previous_release']})
    except Exception as error:
        report['pruned_releases'] = f'failed ({type(error).__name__}: {error})'
    save_report(ssh, report)


if __name__ == '__main__':
    main()
