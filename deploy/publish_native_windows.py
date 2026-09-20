"""Publish an explicitly accepted immutable Windows artifact. Importing never connects."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import sys
import uuid

ROOT = Path(__file__).resolve().parents[1]
REMOTE = '/opt/kiro-byok/downloads/'


def prepare(source, version, acceptance):
    if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]{0,79}', version):
        raise ValueError('Invalid release version')
    data = Path(source).read_bytes()
    if data[:2] != b'MZ':
        raise ValueError('Expected native Windows executable')
    digest = hashlib.sha256(data).hexdigest()
    receipt = json.loads(Path(acceptance).read_text(encoding='utf-8-sig'))
    expected = dict(version=version, sha256=digest, size=len(data), platform='windows', arch='x64')
    if (not isinstance(receipt, dict) or receipt.get('approvedForPublication') is not True
            or any(receipt.get(k) != v for k, v in expected.items())):
        raise ValueError('Acceptance receipt does not approve these exact artifact bytes')
    item = dict(expected, url=f'/downloads/Superkiro-{version}-{digest}-windows-x64.exe',
                signature='unsigned',
                systemRequirements='Windows 10/11 x64 · WebView2 · 单文件免安装 · Rust + Tauri')
    return data, item


def merge_manifest(previous, item):
    if not isinstance(previous, dict) or not isinstance(previous.get('releases'), list):
        raise ValueError('Invalid existing release manifest')
    releases = previous['releases']
    if len(releases) > 100 or any(not isinstance(r, dict) for r in releases):
        raise ValueError('Invalid existing release entries')
    for release in releases:
        same_target = all(release.get(k) == item[k] for k in ('platform', 'arch'))
        if same_target and release.get('version') == item['version'] and release.get('sha256') != item['sha256']:
            raise ValueError('Version already published with a different digest')
    return {'releases': [r for r in releases if
                         (r.get('platform'), r.get('arch')) != (item['platform'], item['arch'])] + [item]}


def validate_history(names, item):
    # Historical immutable artifacts remain authoritative after the current
    # manifest advances to another version. Run under the deployment lock.
    pattern = re.compile(r'Superkiro-' + re.escape(item['version'])
                         + r'-([0-9a-f]{64})-' + re.escape(item['platform'] + '-' + item['arch'])
                         + (r'\.exe' if item['platform'] == 'windows' else r'\.app\.tar\.gz'))
    for name in names:
        match = pattern.fullmatch(name)
        if match and match.group(1) != item['sha256']:
            raise ValueError('Version already published historically with a different digest')


def read_manifest(sftp):
    try:
        with sftp.open(REMOTE + 'releases.json', 'rb') as source:
            data = source.read(1024 * 1024 + 1)
    except OSError as error:
        if error.errno == 2:
            return {'releases': []}
        raise
    if len(data) > 1024 * 1024:
        raise ValueError('Existing manifest too large')
    return json.loads(data)


def publish(ssh, data, item):
    # Lazy imports keep offline regression checks independent of SSH/HTTP packages.
    import requests
    from deploy.release_candidate import deployment_lock, run
    name = item['url'].removeprefix('/downloads/')
    digest = item['sha256']
    temporary = REMOTE + name + '.' + uuid.uuid4().hex + '.tmp'
    manifest_temp = REMOTE + 'releases.' + uuid.uuid4().hex + '.tmp'
    with deployment_lock(ssh):
        with ssh.open_sftp() as sftp:
            validate_history(sftp.listdir(REMOTE), item)
            manifest = merge_manifest(read_manifest(sftp), item)
            try:
                sftp.stat(REMOTE + name)
            except OSError as error:
                if error.errno != 2:
                    raise
                # Upload the approved byte snapshot, never reopen a mutable build path.
                with sftp.open(temporary, 'wb') as target:
                    target.write(data)
                sftp.chmod(temporary, 0o644)
                if run(ssh, 'sha256sum ' + temporary).split()[0] != digest:
                    raise RuntimeError('Uploaded executable hash mismatch')
                # Standard SFTP rename refuses an existing destination.
                sftp.rename(temporary, REMOTE + name)
            else:
                if run(ssh, 'sha256sum ' + REMOTE + name).split()[0] != digest:
                    raise RuntimeError('Immutable artifact conflict; refusing overwrite')
            with sftp.open(manifest_temp, 'wb') as target:
                target.write(json.dumps(manifest, ensure_ascii=False, indent=2).encode())
            sftp.chmod(manifest_temp, 0o644)
            sftp.posix_rename(manifest_temp, REMOTE + 'releases.json')
        with requests.Session() as client:
            client.trust_env = False
            response = client.get('https://kiro.rent/downloads/releases.json', timeout=30, allow_redirects=False)
            if response.status_code != 200 or response.json() != manifest:
                raise RuntimeError('Public manifest mismatch')
            result = hashlib.sha256()
            size = 0
            with client.get('https://kiro.rent' + item['url'], stream=True, timeout=60, allow_redirects=False) as response:
                if response.status_code != 200:
                    raise RuntimeError('Public download unavailable')
                for block in response.iter_content(1024 * 1024):
                    result.update(block)
                    size += len(block)
                    if size > item['size']:
                        raise RuntimeError('Public download exceeds approved size')
            if result.hexdigest() != digest or size != item['size']:
                raise RuntimeError('Public download hash mismatch')
    output = ROOT / '.acceptance/native-published.json'
    output.parent.mkdir(exist_ok=True)
    output.write_text(json.dumps(item, ensure_ascii=False, indent=2), encoding='utf-8')
    print('PASS native download SHA256 ' + digest, flush=True)
    print('https://kiro.rent' + item['url'], flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--version', required=True)
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--acceptance', type=Path, required=True)
    args = parser.parse_args()
    data, item = prepare(args.source, args.version, args.acceptance)
    sys.path.insert(0, str(ROOT))
    from deploy.release_candidate import pinned_connection
    ssh = pinned_connection(json.load(sys.stdin))
    try:
        publish(ssh, data, item)
    finally:
        ssh.close()


if __name__ == '__main__':
    main()
