"""Offline tests only: no production requests, shell commands, or containers."""
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import ssl
import stat
import tempfile
import time
import unittest
import urllib.request
import urllib.response
from email.message import Message
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    'server_backup', Path(__file__).with_name('server-backup.py'))
backup = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(backup)


class AuthTransport(urllib.request.BaseHandler):
    """Mock only HTTPS transport; urllib's actual cookie handling still runs."""
    def __init__(self, failure=None):
        self.requests = []
        self.failure = failure

    def https_open(self, request):
        self.requests.append(request)
        route = request.full_url.rsplit('/api/v1/admin/', 1)[1]
        headers = Message()
        if route == 'session' and request.get_method() == 'POST':
            body = {'success': True}
            headers['Set-Cookie'] = ('__Host-admin_session=secret-cookie; '
                                     'Path=/; Secure; HttpOnly; SameSite=Strict')
        elif route == 'session':
            body = {'success': True, 'csrfToken': 'c' * 64}
        elif route == 'snapshot/sync':
            body = {'status': 'synchronized', 'sequence': 8, 'checksum': 'a' * 64}
        else:
            body = {'success': True}
        if self.failure == route:
            body = {'success': False, 'error': 'secret-response'}
        response = urllib.response.addinfourl(
            io.BytesIO(json.dumps(body).encode()), headers, request.full_url, 200)
        response.msg = 'OK'
        return response


class AuthenticationTests(unittest.TestCase):
    def run_auth(self, transport):
        contexts = []
        def https_open(handler, request):
            contexts.append(handler._context)
            return transport.https_open(request)
        with patch.object(backup, 'read_regular', return_value=json.dumps(
                {'username': 'admin', 'password': 'secret-password'}).encode()), \
                patch.object(urllib.request.HTTPSHandler, 'https_open',
                             autospec=True, side_effect=https_open):
            result = backup.sync_snapshot('https://example.test')
        for context in contexts:
            self.assertEqual(context.verify_mode, ssl.CERT_REQUIRED)
            self.assertTrue(context.check_hostname)
        return result

    def test_totp_rfc_vector_and_invalid_seed(self):
        seed = 'GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ'
        self.assertEqual(backup.totp_code(seed, 59), '287082')
        self.assertEqual(backup.totp_code(seed, 1111111109), '081804')
        for invalid in ('', 'secret', None, 'A' * 33):
            with self.assertRaises(backup.BackupError):
                backup.totp_code(invalid, 59)

    def test_totp_login_only_sends_current_code_never_seed(self):
        seed = 'GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ'
        transport = AuthTransport()
        with patch.object(backup, 'read_regular', return_value=json.dumps(
                {'username': 'admin', 'password': 'secret-password', 'totpSecret': seed}).encode()), \
                patch.object(backup.time, 'time', return_value=59), \
                patch.object(urllib.request.HTTPSHandler, 'https_open', autospec=True,
                             side_effect=lambda _, request: transport.https_open(request)):
            backup.sync_snapshot('https://example.test')
        self.assertEqual(json.loads(transport.requests[0].data)['totpCode'], '287082')
        self.assertNotIn(seed, repr([r.data for r in transport.requests]))

    def test_real_cookie_jar_origin_json_csrf_and_logout(self):
        transport = AuthTransport()
        result = self.run_auth(transport)
        self.assertEqual(result['sequence'], 8)
        self.assertEqual([r.get_method() for r in transport.requests],
                         ['POST', 'GET', 'POST', 'POST'])
        login, session, sync, logout = transport.requests
        self.assertEqual(json.loads(login.data),
                         {'username': 'admin', 'password': 'secret-password'})
        self.assertEqual(login.get_header('Content-type'), 'application/json')
        self.assertEqual(sync.data, b'{}')
        self.assertTrue(logout.full_url.endswith('/session/revoke'))
        for request in transport.requests:
            self.assertEqual(request.get_header('Origin'), 'https://example.test')
            self.assertIsNone(request.get_header('Authorization'))
            self.assertIsNone(request.get_header('X-admin-key'))
            self.assertNotIn('secret-password', request.full_url)
        for request in (session, sync, logout):
            self.assertEqual(request.get_header('Cookie'), '__Host-admin_session=secret-cookie')
        for request in (sync, logout):
            self.assertEqual(request.get_header('X-csrf-token'), 'c' * 64)

    def test_fail_closed_login_session_and_sync(self):
        for failure, expected_requests in [('session', 1), ('snapshot/sync', 4)]:
            with self.subTest(failure=failure):
                transport = AuthTransport(failure)
                with self.assertRaises(backup.BackupError):
                    self.run_auth(transport)
                self.assertEqual(len(transport.requests), expected_requests)
        transport = AuthTransport()
        original = transport.https_open
        def no_csrf(request):
            if request.get_method() == 'GET':
                transport.failure = 'session'
            return original(request)
        transport.https_open = no_csrf
        with self.assertRaises(backup.BackupError):
            self.run_auth(transport)
        self.assertEqual(len(transport.requests), 2)

    def test_bad_origins_rejected_before_credentials(self):
        for origin in ('http://example.test', 'https://example.test/',
                       'https://admin:password@example.test', 'https://example.test?q=1',
                       'https://example.test/#x', 'https://example.test:\n',
                       'https://example.test?', 'https://@example.test'):
            with self.subTest(origin=origin), patch.object(backup, 'read_regular') as read:
                with self.assertRaises((backup.BackupError, ValueError)):
                    backup.sync_snapshot(origin)
                read.assert_not_called()

    def test_redirects_rejected_including_same_origin(self):
        for status in (301, 302, 303, 307, 308):
            with self.assertRaises(backup.BackupError):
                backup.NoRedirect().redirect_request(
                    None, None, status, 'Redirect', {}, 'https://example.test/elsewhere')

    def test_real_opener_does_not_follow_redirects(self):
        transport = AuthTransport()
        def redirect(request):
            transport.requests.append(request)
            headers = Message()
            headers['Location'] = 'https://elsewhere.test/steal'
            response = urllib.response.addinfourl(
                io.BytesIO(b''), headers, request.full_url, 302)
            response.msg = 'Found'
            return response
        transport.https_open = redirect
        with self.assertRaises(backup.BackupError):
            self.run_auth(transport)
        self.assertEqual(len(transport.requests), 1)

    def test_main_error_output_never_contains_exception_secrets(self):
        output = io.StringIO()
        with patch.object(backup, 'sync_snapshot', side_effect=Exception(
                'secret-password secret-cookie secret-response')), \
                patch.object(backup, 'capture_generation') as capture, \
                patch.object(backup.os, 'umask'), patch('sys.stderr', output):
            self.assertEqual(backup.main(), 1)
        self.assertNotIn('secret-', output.getvalue())
        capture.assert_not_called()


class GenerationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.data = self.root / 'data'
        self.data.mkdir()
        self.anchor_path = self.data / 'billing_state.json.anchor'
        self.payload, self.anchor = self.write_generation(8)
        self.synced = {'sequence': 8, 'checksum': self.anchor['checksum']}

    def write_generation(self, sequence):
        payload = json.dumps({'version': 2, 'sequence': sequence,
                              'format': 'kiro-billing-aead-v1', 'ciphertext': 'opaque'}).encode()
        anchor = {'version': 2, 'sequence': sequence,
                  'generation_file': f'billing_state.json.gen_{sequence}',
                  'checksum': hashlib.sha256(payload).hexdigest()}
        (self.data / anchor['generation_file']).write_bytes(payload)
        self.anchor_path.write_text(json.dumps(anchor))
        return payload, anchor

    def capture(self):
        return backup.capture_generation(self.data, self.synced)

    def test_ignores_stale_or_missing_mutable_mirror(self):
        (self.data / 'billing_state.json').write_bytes(b'stale garbage')
        self.assertEqual(self.capture()[0], self.payload)
        (self.data / 'billing_state.json').unlink()
        self.assertEqual(self.capture()[0], self.payload)

    def test_accepts_later_committed_generation(self):
        payload, _ = self.write_generation(9)
        self.assertEqual(self.capture()[0], payload)

    def test_retries_anchor_change_and_pruned_generation(self):
        original = backup.read_regular
        for missing in (False, True):
            self.write_generation(8)
            changed = False
            def racing_read(path, limit, private=False):
                nonlocal changed
                if path.name == 'billing_state.json.gen_8' and not changed:
                    changed = True
                    self.write_generation(9)
                    if missing:
                        raise FileNotFoundError()
                return original(path, limit, private)
            with patch.object(backup, 'read_regular', side_effect=racing_read):
                self.assertEqual(json.loads(self.capture()[0])['sequence'], 9)

    def test_missing_generation_retries_are_bounded(self):
        (self.data / self.anchor['generation_file']).unlink()
        with self.assertRaises(backup.BackupError):
            self.capture()

    def test_rejects_unsafe_legacy_and_invalid_anchors(self):
        for field, value in [('generation_file', '../escape'), ('generation_file', None),
                             ('generation_file', 'billing_state.json.gen_9'),
                             ('sequence', True), ('version', 3), ('checksum', 'bad')]:
            with self.subTest(field=field, value=value):
                self.anchor_path.write_text(json.dumps({**self.anchor, field: value}))
                with self.assertRaises(backup.BackupError):
                    self.capture()

    def test_rejects_corruption_and_inconsistent_sequence(self):
        generation = self.data / self.anchor['generation_file']
        generation.write_bytes(self.payload + b' ')
        with self.assertRaises(backup.BackupError):
            self.capture()
        bad = json.dumps({'version': 2, 'sequence': 7}).encode()
        generation.write_bytes(bad)
        self.anchor_path.write_text(json.dumps({**self.anchor,
            'checksum': hashlib.sha256(bad).hexdigest()}))
        with self.assertRaises(backup.BackupError):
            self.capture()

    def test_rejects_pre_sync_or_divergent_state(self):
        for sequence, digest in ((9, self.anchor['checksum']), (8, 'f' * 64)):
            self.synced = {'sequence': sequence, 'checksum': digest}
            with self.assertRaises(backup.BackupError):
                self.capture()

    def publish(self):
        # Production runs on Linux. Windows cannot fsync directories or express
        # POSIX owner-only modes; exercise publication there with those calls mocked.
        with patch.object(backup.os, 'geteuid', return_value=self.root.stat().st_uid, create=True):
            if os.name == 'nt':
                with patch.object(backup, 'fsync_directory'):
                    return backup.publish_bundle(self.root / 'backups', *self.capture())
            return backup.publish_bundle(self.root / 'backups', *self.capture())

    def test_restore_bundle_self_contained_hashes_permissions_and_isolation(self):
        first = self.publish()
        second = self.publish()
        self.assertNotEqual(first.parent, second.parent)
        manifest = json.loads(first.read_bytes())
        self.assertEqual(manifest['status'], 'completed')
        for key in ('snapshot', 'anchor'):
            content = (first.parent / manifest[key + '_file']).read_bytes()
            self.assertEqual(hashlib.sha256(content).hexdigest(), manifest[key + '_sha256'])
        self.assertEqual((first.parent / self.anchor['generation_file']).read_bytes(), self.payload)
        self.assertEqual((first.parent / (manifest['snapshot_file'] + '.sha256')).read_text(),
                         f"{self.anchor['checksum']}  {manifest['snapshot_file']}\n")
        self.assertFalse(list(first.parent.parent.glob('.server-backup_*')))
        if os.name != 'nt':
            self.assertEqual(stat.S_IMODE(first.parent.stat().st_mode), 0o700)
            self.assertEqual(stat.S_IMODE(first.parent.parent.stat().st_mode), 0o700)
            for path in first.parent.iterdir():
                self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)

    def test_main_orders_sync_capture_publish_then_retention(self):
        events = []
        def sync(origin):
            self.assertEqual(origin, 'https://kiro.rent')
            events.append('sync')
            return self.synced
        capture = backup.capture_generation
        def capture_after_sync(data_dir, synced):
            self.assertEqual(events, ['sync'])
            events.append('capture')
            return capture(data_dir, synced)
        publish = backup.publish_bundle
        def publish_after_capture(*args):
            self.assertEqual(events, ['sync', 'capture'])
            events.append('publish')
            return publish(*args)
        def prune_after_publish(directory, days):
            self.assertEqual(events, ['sync', 'capture', 'publish'])
            self.assertEqual(days, 7)
            self.assertEqual(len(list(directory.glob('server-backup_*/*.manifest.json'))), 1)
        with patch.object(backup, 'ROOT', self.root), \
                patch.dict(os.environ, {}, clear=True), \
                patch.object(backup, 'sync_snapshot', side_effect=sync), \
                patch.object(backup, 'capture_generation', side_effect=capture_after_sync), \
                patch.object(backup, 'publish_bundle', side_effect=publish_after_capture), \
                patch.object(backup, 'prune_bundles', side_effect=prune_after_publish), \
                patch.object(backup.os, 'geteuid', return_value=self.root.stat().st_uid, create=True), \
                patch.object(backup.os, 'umask'), \
                patch.object(backup, 'fsync_directory'), patch('sys.stdout', io.StringIO()):
            self.assertEqual(backup.main(), 0)

    def test_rename_failure_never_publishes_bundle(self):
        with patch.object(Path, 'rename', side_effect=OSError('rename failed')):
            with self.assertRaises(OSError):
                self.publish()
        self.assertEqual(list((self.root / 'backups').iterdir()), [])

    def test_failed_write_never_publishes_bundle(self):
        with patch.object(backup.os, 'fsync', side_effect=OSError('disk full')):
            with self.assertRaises(OSError):
                self.publish()
        self.assertEqual(list((self.root / 'backups').iterdir()), [])

    def test_offline_bundle_verification_is_not_restore_acceptance(self):
        manifest = self.publish()
        result = backup.verify_bundle(manifest)
        self.assertEqual(result['sequence'], self.anchor['sequence'])
        self.assertEqual(result['verification'], 'byte-integrity-only')
        self.assertFalse(result['restore_verified'])

    def test_verifier_rejects_each_missing_bundle_component(self):
        for key in ('snapshot_file', 'anchor_file', 'generation', 'sidecar'):
            manifest = self.publish()
            meta = json.loads(manifest.read_bytes())
            name = (self.anchor['generation_file'] if key == 'generation' else
                    meta['snapshot_file'] + '.sha256' if key == 'sidecar' else meta[key])
            (manifest.parent / name).unlink()
            with self.assertRaises((OSError, backup.BackupError)):
                backup.verify_bundle(manifest)

    def test_verifier_rejects_corruption_and_unsafe_paths(self):
        for key, value in [('status', 'started'), ('snapshot_file', '../outside.json'),
                           ('anchor_file', '/outside'), ('snapshot_sha256', '0' * 64),
                           ('anchor_sha256', '0' * 64)]:
            manifest = self.publish()
            meta = json.loads(manifest.read_bytes())
            meta[key] = value
            manifest.write_text(json.dumps(meta))
            with self.assertRaises(backup.BackupError):
                backup.verify_bundle(manifest)
        for kind in ('generation', 'sidecar', 'snapshot'):
            manifest = self.publish()
            meta = json.loads(manifest.read_bytes())
            name = (self.anchor['generation_file'] if kind == 'generation' else
                    meta['snapshot_file'] + '.sha256' if kind == 'sidecar' else meta['snapshot_file'])
            (manifest.parent / name).write_bytes(b'corrupt')
            with self.assertRaises(backup.BackupError):
                backup.verify_bundle(manifest)

    def test_retention_preserves_invalid_or_incomplete_bundles(self):
        for content in ('{}', 'not json', '{"status":"started"}', 'null'):
            manifest = self.publish()
            manifest.write_text(content)
            old = time.time() - 9 * 86400
            os.utime(manifest.parent, (old, old))
            with patch.object(backup, 'fsync_directory'), patch('sys.stderr', io.StringIO()):
                backup.prune_bundles(manifest.parent.parent, 7)
            self.assertTrue(manifest.exists())

    def test_retention_removes_only_old_runner_bundles(self):
        expired = self.publish()
        fresh = self.publish()
        old = time.time() - 9 * 86400
        os.utime(expired.parent, (old, old))
        legacy = fresh.parent.parent / 'billing_state_old.manifest.json'
        legacy.write_text('{}')
        with patch.object(backup, 'fsync_directory'):
            backup.prune_bundles(fresh.parent.parent, 7)
        self.assertFalse(expired.parent.exists())
        self.assertTrue(fresh.exists())
        self.assertTrue(legacy.exists())

    @unittest.skipIf(os.name == 'nt', 'POSIX file modes and no-follow descriptors')
    def test_credentials_permissions_symlinks_and_read_limits(self):
        credentials = self.root / 'credentials.json'
        credentials.write_bytes(b'{}')
        credentials.chmod(0o644)
        with self.assertRaises(backup.BackupError):
            backup.read_regular(credentials, 4096, private=True)
        credentials.chmod(0o600)
        self.assertEqual(backup.read_regular(credentials, 4096, private=True), b'{}')
        with self.assertRaises(backup.BackupError):
            backup.read_regular(credentials, 1)
        link = self.root / 'link'
        link.symlink_to(credentials)
        with self.assertRaises(OSError):
            backup.read_regular(link, 4096, private=True)


if __name__ == '__main__':
    unittest.main()
