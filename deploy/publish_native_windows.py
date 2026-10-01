"""Publish an explicitly accepted immutable Windows artifact. Importing never connects.

The release entry is signed with the offline update key (update_signing.py): installed
clients update themselves to it, and only because of that signature. Releases are
mandatory updates unless published with --optional.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import sys
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parent))
import update_signing

ROOT = Path(__file__).resolve().parents[1]
REMOTE = '/opt/kiro-byok/downloads/'


def check_version(version):
    # Clients compare versions numerically, and each knows its own from its build.
    if not isinstance(version, str) or not update_signing.RELEASE_VERSION.fullmatch(version):
        raise ValueError('Release version must be MAJOR.MINOR.PATCH, e.g. 0.1.1')


def publication_metadata(item, receipt):
    """Disclose informed beta authorization without claiming native acceptance."""
    basis = receipt.get('approvalBasis')
    if basis == 'user-directed-beta-publication':
        if (receipt.get('runtimeAcceptance') is not False
                or not re.fullmatch(r'[0-9a-f]{40}', str(receipt.get('sourceCommit', '')))
                or not re.fullmatch(r'[0-9]+', str(receipt.get('buildRun', '')))):
            raise ValueError('Beta approval requires source/run identity and runtimeAcceptance=false')
        item = dict(item, approvalBasis=basis, runtimeAcceptance=False,
                    sourceCommit=receipt['sourceCommit'], buildRun=str(receipt['buildRun']),
                    channel='beta', systemRequirements=item['systemRequirements']
                    + ' · 测试版 · 真机运行验收未完成')
    elif receipt.get('runtimeAcceptance') is False:
        raise ValueError('Missing explicit beta publication authorization')
    return item


def prepare(source, version, acceptance, key, mandatory=True, *, unpacked_source=None, unpacked_provenance=None):
    check_version(version)
    data = Path(source).read_bytes()
    if data[:2] != b'MZ':
        raise ValueError('Expected native Windows executable')
    receipt = json.loads(Path(acceptance).read_text(encoding='utf-8-sig'))
    packing = None
    identity = data
    if unpacked_source is not None or unpacked_provenance is not None:
        from packed_windows import verify_packed
        identity, packing = verify_packed(data, version, receipt, unpacked_source, unpacked_provenance)
    elif isinstance(receipt, dict) and 'packing' in receipt:
        raise ValueError('Packed approval requires the original CI artifact and provenance')
    # Built as another version, the client would take this release for newer than itself
    # and install it again at every start.
    if identity.count(update_signing.release_marker(version)) != 1:
        raise ValueError('The executable was not built as this release '
                         '(set SUPERKIRO_RELEASE_VERSION to it when building)')
    # A debug build trusts a test key anyone can derive and a redirecting variable.
    if update_signing.debug_build(identity) or update_signing.debug_build(data):
        raise ValueError('A debug build cannot be published; build with --release')
    digest = hashlib.sha256(data).hexdigest()
    expected = dict(version=version, sha256=digest, size=len(data), platform='windows', arch='x64')
    if (not isinstance(receipt, dict) or receipt.get('approvedForPublication') is not True
            or any(receipt.get(k) != v for k, v in expected.items())):
        raise ValueError('Acceptance receipt does not approve these exact artifact bytes')
    item = dict(expected, url=f'/downloads/Superkiro-{version}-Windows.exe',
                signature='unsigned',
                systemRequirements='Windows 10/11 x64 · WebView2 · 单文件免安装 · Rust + Tauri')
    if packing is not None:
        item.update(packaging='user-supplied-packed', originalSha256=packing['originalSha256'])
    return data, update_signing.signed_entry(publication_metadata(item, receipt), key, mandatory)


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
        # Clients never go back from what they may have installed. An entry without an
        # update signature predates self-update: no client installed it by updating.
        if (same_target and 'updateSignature' in release
                and newer(release.get('version'), item['version'])):
            raise ValueError('A newer version is already published; clients never go back')
    return {'releases': [r for r in releases if
                         (r.get('platform'), r.get('arch')) != (item['platform'], item['arch'])] + [item]}


def newer(candidate, current):
    """Whether numeric version `candidate` is later than `current`, as clients compare."""
    def parts(version):
        if not isinstance(version, str) or not update_signing.VERSION.fullmatch(version):
            return None
        numbers = [int(p) for p in version.split('.')]
        return numbers + [0] * (4 - len(numbers))
    a, b = parts(candidate), parts(current)
    return a is not None and b is not None and a > b


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
    parser.add_argument('--unpacked-source', type=Path, help='Original unmodified CI EXE for an explicitly approved packed derivative')
    parser.add_argument('--unpacked-provenance', type=Path, help='Original CI provenance, bound by SHA256 in the packing receipt')
    parser.add_argument('--update-key', type=Path, default=update_signing.KEY)
    parser.add_argument('--optional', action='store_true',
                        help='let installed clients postpone this update')
    args = parser.parse_args()
    data, item = prepare(args.source, args.version, args.acceptance,
                         update_signing.load(args.update_key), mandatory=not args.optional,
                         unpacked_source=args.unpacked_source, unpacked_provenance=args.unpacked_provenance)
    sys.path.insert(0, str(ROOT))
    from deploy.release_candidate import pinned_connection
    ssh = pinned_connection(json.load(sys.stdin))
    try:
        publish(ssh, data, item)
    finally:
        ssh.close()


if __name__ == '__main__':
    main()
