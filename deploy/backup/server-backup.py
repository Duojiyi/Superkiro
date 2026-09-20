#!/usr/bin/env python3
"""Online, cookie-authenticated billing backup. No shell or container operations.

Each server-backup_* directory is a self-contained restore.sh bundle. Only a
validated, post-sync anchored generation is copied, never the mutable mirror.
This checks transport/byte integrity, not AEAD or Rust ledger invariants; restore
still requires the gateway verifier and the separately protected master key.
"""
import base64
import hashlib
import hmac
import http.cookiejar
import json
import os
from pathlib import Path
import re
import shutil
import ssl
import stat
import sys
import tempfile
import time
import urllib.parse
import urllib.request
import uuid

ROOT = Path('/opt/kiro-byok')
CREDENTIALS = Path('/etc/kiro-byok/admin-access.json')
MAX_SNAPSHOT_BYTES = 256 * 1024 * 1024


class BackupError(Exception):
    """Safe, non-secret diagnostic for the service log."""


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise BackupError('Administrator redirects are forbidden')


def read_regular(path, limit, private=False):
    fd = os.open(path, os.O_RDONLY | getattr(os, 'O_NOFOLLOW', 0)
                 | getattr(os, 'O_NONBLOCK', 0))
    with os.fdopen(fd, 'rb') as source:
        info = os.fstat(source.fileno())
        if not stat.S_ISREG(info.st_mode):
            raise BackupError('Expected a regular input file')
        if private and (info.st_mode & 0o077 or
                        info.st_uid not in (0, os.geteuid())):
            raise BackupError('Credentials must be owner-only and owned by root or the runner')
        content = source.read(limit + 1)
    if len(content) > limit:
        raise BackupError('Input exceeds its size limit')
    return content


def valid_sequence(value):
    return type(value) is int and 0 < value < 2 ** 64


def valid_checksum(value):
    return isinstance(value, str) and re.fullmatch(r'[0-9a-f]{64}', value) is not None


def totp_code(secret, now):
    # RFC 6238, matching the gateway: SHA-1, six digits, 30-second period.
    if not isinstance(secret, str) or not re.fullmatch(r'[A-Z2-7]{32,128}', secret):
        raise BackupError('Invalid TOTP credential')
    try:
        key = base64.b32decode(secret + '=' * ((-len(secret)) % 8))
    except ValueError:
        raise BackupError('Invalid TOTP credential') from None
    digest = hmac.new(key, (int(now) // 30).to_bytes(8, 'big'), hashlib.sha1).digest()
    offset = digest[-1] & 15
    value = int.from_bytes(digest[offset:offset + 4], 'big') & 0x7fffffff
    return f'{value % 1000000:06d}'


def sync_snapshot(origin, credentials=CREDENTIALS):
    parsed = urllib.parse.urlsplit(origin)
    if (parsed.scheme != 'https' or not parsed.hostname or parsed.username is not None
            or parsed.password is not None or origin != 'https://' + parsed.netloc
            or any(c.isspace() for c in origin) or parsed.netloc.endswith(':')):
        raise BackupError('ADMIN_ORIGIN must be an exact HTTPS origin')
    parsed.port  # Reject malformed ports before loading credentials.
    login = json.loads(read_regular(credentials, 4096, private=True))
    if (not isinstance(login, dict) or
            any(not isinstance(login.get(k), str) or not login[k]
                for k in ('username', 'password'))):
        raise BackupError('Invalid administrator credentials file')
    jar = http.cookiejar.CookieJar()
    opener = urllib.request.build_opener(
        urllib.request.ProxyHandler({}), NoRedirect(),
        urllib.request.HTTPSHandler(context=ssl.create_default_context()),
        urllib.request.HTTPCookieProcessor(jar))

    def request(path, body=None, csrf=None):
        headers = {'Origin': origin, 'Accept': 'application/json'}
        data = None
        if body is not None:
            data = json.dumps(body).encode('utf-8')
            headers['Content-Type'] = 'application/json'
        if csrf:
            headers['x-csrf-token'] = csrf
        req = urllib.request.Request(origin + '/api/v1/admin/' + path,
                                     data=data, headers=headers)
        with opener.open(req, timeout=30) as response:
            if response.status != 200:
                raise BackupError('Administrator request failed')
            raw = response.read(65537)
            if len(raw) > 65536:
                raise BackupError('Administrator response exceeds its size limit')
            result = json.loads(raw)
        if not isinstance(result, dict):
            raise BackupError('Invalid administrator response')
        return result

    csrf = None
    try:
        body = {k: login[k] for k in ('username', 'password')}
        if 'totpSecret' in login:
            body['totpCode'] = totp_code(login['totpSecret'], time.time())
        if request('session', body).get('success') is not True:
            raise BackupError('Administrator login failed')
        session = request('session')
        csrf = session.get('csrfToken')
        if session.get('success') is not True or not valid_checksum(csrf):
            csrf = None
            raise BackupError('Administrator session has no valid CSRF token')
        synced = request('snapshot/sync', {}, csrf)
        if (synced.get('status') != 'synchronized'
                or not valid_sequence(synced.get('sequence'))
                or not valid_checksum(synced.get('checksum'))):
            raise BackupError('Snapshot synchronization was not confirmed')
        return synced
    finally:
        if csrf:
            try:
                request('session/revoke', {}, csrf)
            except Exception:
                # A failed logout cannot invalidate a successful disk sync. The
                # server expires this in-memory-only session after 15 minutes.
                print('Warning: administrator session revocation failed', file=sys.stderr)
        jar.clear()


def capture_generation(data_dir, synced, attempts=5):
    anchor_path = data_dir / 'billing_state.json.anchor'
    for _ in range(attempts):
        try:
            anchor_bytes = read_regular(anchor_path, 4096)
            anchor = json.loads(anchor_bytes)
            sequence = anchor['sequence']
            generation = anchor.get('generation_file')
            if (anchor.get('version') != 2 or not valid_sequence(sequence)
                    or generation != f'billing_state.json.gen_{sequence}'
                    or not valid_checksum(anchor.get('checksum'))):
                raise BackupError('Invalid committed generation anchor')
            payload = read_regular(data_dir / generation, MAX_SNAPSHOT_BYTES)
            if read_regular(anchor_path, 4096) != anchor_bytes:
                continue  # Writer committed or pruned during capture; start again.
            digest = hashlib.sha256(payload).hexdigest()
            snapshot = json.loads(payload)
            if (digest != anchor['checksum'] or not valid_sequence(snapshot.get('sequence'))
                    or snapshot.get('sequence') != sequence
                    or snapshot.get('version') != anchor['version']):
                raise BackupError('Generation does not match its anchor')
            if sequence < synced['sequence'] or (sequence == synced['sequence']
                                                and digest != synced['checksum']):
                raise BackupError('Local generation does not match the synchronized state')
            return payload, anchor_bytes, generation
        except FileNotFoundError:
            continue  # The writer retains only the newest two generations.
    raise BackupError('Could not capture a stable committed generation')


def fsync_directory(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def publish_bundle(backup_dir, payload, anchor, generation):
    backup_dir.mkdir(mode=0o700, parents=True, exist_ok=True)
    info = backup_dir.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.geteuid():
        raise BackupError('Backup directory must be a real directory owned by the runner')
    backup_dir.chmod(0o700)
    identifier = time.strftime('%Y%m%d_%H%M%S', time.gmtime()) + '_' + uuid.uuid4().hex
    prefix = 'billing_state_' + identifier
    snapshot_name = prefix + '.json'
    digest = hashlib.sha256(payload).hexdigest()
    manifest = {
        'version': 1, 'generation_id': identifier,
        'created_at': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()),
        'snapshot_file': snapshot_name, 'snapshot_sha256': digest,
        'anchor_file': snapshot_name + '.anchor',
        'anchor_sha256': hashlib.sha256(anchor).hexdigest(), 'status': 'completed',
    }
    final = backup_dir / ('server-backup_' + identifier)
    with tempfile.TemporaryDirectory(prefix='.server-backup_', dir=backup_dir) as staging:
        stage = Path(staging)
        # Insertion order makes the manifest the last file written. Publishing
        # the entire directory atomically also isolates identical generation names.
        files = {
            snapshot_name: payload, snapshot_name + '.anchor': anchor,
            generation: payload,
            snapshot_name + '.sha256': f'{digest}  {snapshot_name}\n'.encode(),
            prefix + '.manifest.json': json.dumps(manifest, indent=2).encode(),
        }
        for name, content in files.items():
            fd = os.open(stage / name, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(fd, 'wb') as target:
                target.write(content)
                target.flush()
                os.fsync(target.fileno())
        fsync_directory(stage)
        stage.rename(final)
        fsync_directory(backup_dir)
    return final / (prefix + '.manifest.json')


def verify_bundle(manifest_path):
    """Offline byte-integrity check only; no keys, network, restore or AEAD claim."""
    def read(name, limit):
        if not isinstance(name, str) or not re.fullmatch(r'[A-Za-z0-9_.-]+', name) or name in ('.', '..'):
            raise BackupError('Unsafe bundle filename')
        path = manifest_path.parent / name
        if path.is_symlink():
            raise BackupError('Bundle symlinks are forbidden')
        return read_regular(path, limit)

    manifest = json.loads(read(manifest_path.name, 16384))
    if manifest.get('version') != 1 or manifest.get('status') != 'completed':
        raise BackupError('Backup manifest is not completed version 1')
    snapshot_name = manifest.get('snapshot_file')
    if (not isinstance(snapshot_name, str) or not snapshot_name.endswith('.json')
            or manifest_path.name != snapshot_name[:-5] + '.manifest.json'
            or manifest.get('anchor_file') != snapshot_name + '.anchor'):
        raise BackupError('Bundle filenames do not match')
    payload = read(snapshot_name, MAX_SNAPSHOT_BYTES)
    anchor_bytes = read(manifest['anchor_file'], 4096)
    digest = hashlib.sha256(payload).hexdigest()
    if (manifest.get('snapshot_sha256') != digest
            or manifest.get('anchor_sha256') != hashlib.sha256(anchor_bytes).hexdigest()):
        raise BackupError('Bundle manifest hash mismatch')
    snapshot, anchor = json.loads(payload), json.loads(anchor_bytes)
    sequence = anchor.get('sequence')
    if (not valid_sequence(sequence) or anchor.get('version') != 2
            or snapshot.get('version') != 2 or not valid_sequence(snapshot.get('sequence'))
            or snapshot['sequence'] != sequence or anchor.get('checksum') != digest
            or anchor.get('generation_file') != f'billing_state.json.gen_{sequence}'):
        raise BackupError('Bundle anchor does not match snapshot')
    if read(anchor['generation_file'], MAX_SNAPSHOT_BYTES) != payload:
        raise BackupError('Bundle generation mismatch')
    if read(snapshot_name + '.sha256', 4096) != f'{digest}  {snapshot_name}\n'.encode():
        raise BackupError('Bundle checksum sidecar mismatch')
    return {'sequence': sequence, 'snapshot_sha256': digest,
            'verification': 'byte-integrity-only', 'restore_verified': False}


def prune_bundles(backup_dir, retention_days):
    cutoff = time.time() - retention_days * 86400
    for path in backup_dir.iterdir():
        # Only our complete directory bundles; never touch old shell backups,
        # loose generation files, symlinks, or interrupted staging directories.
        if not re.fullmatch(r'server-backup_\d{8}_\d{6}_[0-9a-f]{32}', path.name):
            continue
        info = path.lstat()
        if not stat.S_ISDIR(info.st_mode) or info.st_mtime >= cutoff:
            continue
        identifier = path.name.removeprefix('server-backup_')
        manifest = path / ('billing_state_' + identifier + '.manifest.json')
        try:
            verify_bundle(manifest)
        except (OSError, ValueError, KeyError, TypeError, AttributeError, BackupError):
            print('Retention preserved an invalid/incomplete bundle; operator review required', file=sys.stderr)
            continue
        shutil.rmtree(path)
    fsync_directory(backup_dir)


def main():
    os.umask(0o077)
    try:
        retention = int(os.environ.get('RETENTION_DAYS', '7'))
        if retention < 1:
            raise BackupError('RETENTION_DAYS must be positive')
        synced = sync_snapshot(os.environ.get('ADMIN_ORIGIN', 'https://kiro.rent'))
        payload, anchor, generation = capture_generation(ROOT / 'data', synced)
        manifest = publish_bundle(ROOT / 'backups', payload, anchor, generation)
        prune_bundles(ROOT / 'backups', retention)
    except Exception:
        # Never print urllib exceptions, response bodies, credentials, or cookies.
        print('Backup failed: authentication, sync, capture, or publication did not complete',
              file=sys.stderr)
        return 1
    print(f'Backup completed: {manifest}')
    return 0


if __name__ == '__main__':
    sys.exit(main())
