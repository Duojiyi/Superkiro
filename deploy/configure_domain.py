"""Update public ingress under the release lock; never modify gateway data."""
from pathlib import Path
from datetime import datetime, timezone
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from deploy.release_candidate import deployment_lock, external_readiness, release_path, run, write_remote

ROOT = Path(__file__).resolve().parents[1]


def apply(ssh):
    with deployment_lock(ssh):
        current = release_path(run(ssh, 'readlink -f /opt/kiro-byok/current'))
        stamp = datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ')
        backup = '/opt/kiro-byok/backups/domain-' + stamp
        run(ssh, f'mkdir -m 700 {backup}\ncp -a {current}/deploy {backup}/deploy\n'
            f'diff -qr {current}/deploy {backup}/deploy\nmkdir -p /opt/kiro-byok/downloads')
        cmd = f'docker compose -p deploy -f {current}/deploy/docker-compose.ip.yml'
        with ssh.open_sftp() as sftp:
            compose = sftp.open(current + '/deploy/docker-compose.ip.yml').read().decode()
        mount = '      - /opt/kiro-byok/downloads:/app/downloads:ro\n'
        if mount not in compose:
            anchor = '      - caddy_data:/data\n'
            if compose.count(anchor) != 1:
                raise RuntimeError('Unexpected Compose mount structure')
            compose = compose.replace(anchor, mount + anchor)
        if compose.count(mount) != 1:
            raise RuntimeError('Unexpected download mount count')
        # Validate exactly the bytes that will be installed, using release-relative mounts.
        write_remote(ssh, current + '/deploy/Caddyfile.ip.next', (ROOT / 'deploy/Caddyfile.ip').read_bytes())
        write_remote(ssh, current + '/deploy/compose.next.yml', compose.encode())
        run(ssh, f'docker cp {current}/deploy/Caddyfile.ip.next kiro-caddy:/tmp/superkiro-domain.caddy')
        run(ssh, 'docker exec kiro-caddy caddy validate --config /tmp/superkiro-domain.caddy --adapter caddyfile')
        run(ssh, f'docker compose -p deploy -f {current}/deploy/compose.next.yml config --quiet')
        try:
            run(ssh, f'mv {current}/deploy/Caddyfile.ip.next {current}/deploy/Caddyfile.ip\n'
                f'mv {current}/deploy/compose.next.yml {current}/deploy/docker-compose.ip.yml')
            run(ssh, cmd + ' up -d --no-deps --pull never --force-recreate caddy')
            external_readiness()
            if run(ssh, "docker inspect kiro-caddy --format '{{.State.Running}}'") != 'true':
                raise RuntimeError('Ingress stopped during readiness')
        except Exception:
            try:
                run(ssh, f'cp -a {backup}/deploy/. {current}/deploy/')
                run(ssh, cmd + ' up -d --no-deps --pull never --force-recreate caddy')
                external_readiness()
            except Exception:
                raise RuntimeError('Ingress rollback failed; backup preserved for operator recovery') from None
            raise RuntimeError('Ingress update failed; previous configuration restored') from None
    print('DOMAIN_INGRESS_VERIFIED', backup)
