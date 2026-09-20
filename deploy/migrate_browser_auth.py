"""Explicit, pinned Basic-to-cookie configuration migration; no secret output."""
import json
import re
import shlex
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from deploy.release_candidate import (ROOT, SOURCE_PATHS, configuration_digest, deployment_lock,
    pinned_connection, release_path, run, save_report, tree_digest, write_remote)


def parse_env(text):
    return {k: v.strip().strip("'\"") for line in text.splitlines()
            if line and not line.startswith('#') and '=' in line
            for k, v in [line.split('=', 1)]}


def verify_staged(ssh, report):
    dest = release_path('/opt/kiro-byok/releases/' + report['release'])
    if tree_digest(ssh, dest + '/auth-migration', ['gateway.env', 'caddy.env', 'compose.verify.yml']) != report['browser_auth_migration']['sha256']:
        raise RuntimeError('Staged authentication configuration changed')


def apply_staged(ssh, report):
    verify_staged(ssh, report)
    if not report['browser_auth_migration'].get('restore_verified'):
        raise RuntimeError('Isolated restore verification required')
    dest = release_path('/opt/kiro-byok/releases/' + report['release'])
    run(ssh, f'cp -a {dest}/auth-migration/gateway.env /etc/kiro-byok/gateway.env\n'
             f'cp -a {dest}/auth-migration/caddy.env /etc/kiro-byok/caddy.env\n'
             f'docker compose -p deploy -f {dest}/deploy/docker-compose.ip.yml config --quiet')
    report['migrated_configuration_sha256'] = configuration_digest(ssh, dest)
    save_report(ssh, report)


def stage(ssh):
    report = json.loads((ROOT / 'deployment-candidate-results.json').read_text())
    if report['status'] != 'staging' or report.get('browser_auth_migration'):
        raise RuntimeError('Expected a fresh staged candidate')
    old = release_path(report['previous_release'])
    dest = release_path('/opt/kiro-byok/releases/' + report['release'])
    if configuration_digest(ssh, old) != report['configuration_sha256']:
        raise RuntimeError('Production configuration changed')
    if tree_digest(ssh, dest, SOURCE_PATHS + ['deploy/docker-compose.ip.yml', 'deploy/Caddyfile.ip']) != report['candidate_sha256']:
        raise RuntimeError('Candidate changed')
    with ssh.open_sftp() as s:
        gateway = s.open('/etc/kiro-byok/gateway.env').read().decode()
        caddy = s.open('/etc/kiro-byok/caddy.env').read().decode()
    env = parse_env(gateway)
    password_hash = parse_env(caddy).get('ADMIN_PASSWORD_HASH', '')
    if not re.fullmatch(r'\$2[aby]\$\d\d\$[./A-Za-z0-9]{53}', password_hash):
        raise RuntimeError('Existing bcrypt hash invalid')
    # Retain every unrelated setting, especially KEK/upstreams/admin signing key.
    additions = {'ADMIN_BROWSER_LOGIN':'true', 'ADMIN_ORIGIN':'https://kiro.rent', 'ADMIN_PASSWORD_HASH':"'"+password_hash+"'"}
    gateway = '\n'.join(l for l in gateway.splitlines() if l.split('=',1)[0] not in additions) + '\n'
    gateway += ''.join(k+'='+v+'\n' for k,v in additions.items())
    caddy = '\n'.join(l for l in caddy.splitlines() if l.split('=',1)[0] != 'ADMIN_PASSWORD_HASH')+'\n'
    new = parse_env(gateway)
    if any(new.get(k)!=v for k,v in env.items() if k not in additions):
        raise RuntimeError('Unrelated configuration changed')
    run(ssh, f'mkdir -m 700 {dest}/auth-migration')
    write_remote(ssh, dest+'/auth-migration/gateway.env', gateway.encode())
    write_remote(ssh, dest+'/auth-migration/caddy.env', caddy.encode())
    write_remote(ssh, dest+'/deploy/Caddyfile.ip', (ROOT/'deploy/Caddyfile.ip').read_bytes())
    effective = json.loads(run(ssh, f'docker compose -p deploy -f {dest}/deploy/docker-compose.ip.yml config --format json'))
    caddy_image = shlex.quote(effective['services']['caddy']['image'])
    run(ssh, f'docker run --rm --network none --env-file {dest}/auth-migration/caddy.env '
        f'-v {dest}/deploy/Caddyfile.ip:/etc/caddy/Caddyfile:ro {caddy_image} caddy validate --config /etc/caddy/Caddyfile >/dev/null')
    with ssh.open_sftp() as s:
        compose = s.open(dest + '/deploy/docker-compose.ip.yml').read().decode()
    compose = compose.replace('/etc/kiro-byok/gateway.env', dest + '/auth-migration/gateway.env').replace('/etc/kiro-byok/caddy.env', dest + '/auth-migration/caddy.env')
    write_remote(ssh, dest + '/auth-migration/compose.verify.yml', compose.encode())
    report['pre_migration_candidate_sha256']=report['candidate_sha256']
    report['candidate_sha256']=tree_digest(ssh,dest,SOURCE_PATHS+['deploy/docker-compose.ip.yml','deploy/Caddyfile.ip'])
    report['browser_auth_migration']={'sha256':tree_digest(ssh,dest+'/auth-migration',['gateway.env','caddy.env','compose.verify.yml']),
        'restore_verified':False,'preserved_existing_secrets':True,'approved_change':'Basic ingress to gateway Cookie/CSRF form login'}
    save_report(ssh,report)
    print('PASS migration staged, original secrets retained; production unchanged',flush=True)


if __name__ == '__main__':
    ssh=pinned_connection(json.load(sys.stdin))
    try:
        with deployment_lock(ssh):stage(ssh)
    finally:ssh.close()


def rehearse(ssh, report):
    dest = release_path('/opt/kiro-byok/releases/' + report['release'])
    script = (ROOT / 'deploy/rehearse_auth_migration.py').read_bytes()
    write_remote(ssh, dest + '/rehearse-auth.py', script)
    network = 'superkiro-recovery-' + report['release'].lower()
    run(ssh, f'docker network create --internal {network}')
    try:
        result = json.loads(run(ssh, f'timeout --kill-after=10s 240s python3 {dest}/rehearse-auth.py {dest}', timeout=280))
    finally:
        for label in ('old', 'new'):
            name = 'superkiro-recovery-' + label + '-' + report['release'].lower()
            run(ssh, f'if docker container inspect {name} >/dev/null 2>&1; then docker rm -f {name}; fi')
        run(ssh, f'docker network rm {network}')
    report['browser_auth_migration']['restore_verified'] = True
    report['browser_auth_migration']['restore_result'] = result
    save_report(ssh, report)
