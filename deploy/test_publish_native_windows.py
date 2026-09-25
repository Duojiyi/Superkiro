"""Offline publication regression checks; never imports the production SSH helper."""
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from contextlib import nullcontext
from types import SimpleNamespace
from unittest.mock import MagicMock, patch
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from publish_native_windows import prepare, merge_manifest, validate_history, check_version
import update_signing

FIXTURE = b'MZfixture-not-executable' + update_signing.release_marker('1.2.3')


class PublicationTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name)
        self.exe, self.receipt = root / 'app.exe', root / 'accepted.json'
        self.exe.write_bytes(FIXTURE)
        # A key the client under test trusts, standing in for the offline release key.
        self.key = Ed25519PrivateKey.generate()
        trusted = patch('update_signing.client_keys',
                        return_value=[update_signing.public_hex(self.key)])
        trusted.start()
        self.addCleanup(trusted.stop)
        self.approval = dict(approvedForPublication=True, version='1.2.3',
                            sha256=hashlib.sha256(self.exe.read_bytes()).hexdigest(),
                            size=self.exe.stat().st_size, platform='windows', arch='x64')
        self.save()

    def save(self):
        self.receipt.write_text(json.dumps(self.approval), encoding='utf-8')

    def prepare(self, **options):
        return prepare(self.exe, '1.2.3', self.receipt, self.key, **options)

    def test_exact_approved_bytes_and_immutable_url(self):
        data, item = self.prepare()
        self.assertEqual(item['url'], '/downloads/Superkiro-1.2.3-Windows.exe')
        self.exe.write_bytes(b'MZchanged-build' + update_signing.release_marker('1.2.3'))
        self.assertEqual(data, FIXTURE)
        with self.assertRaises(ValueError):
            self.prepare()

    def test_entry_is_signed_for_clients_and_mandatory_by_default(self):
        _, item = self.prepare()
        self.assertTrue(item['mandatory'])
        self.assertTrue(update_signing.verify(item, item['updateSignature'],
                                              [update_signing.public_hex(self.key)]))
        # The signature covers what the client acts on; the rest of the entry stays as it was.
        self.assertFalse(update_signing.verify(dict(item, version='1.2.4'), item['updateSignature'],
                                               [update_signing.public_hex(self.key)]))
        self.assertEqual(item['signature'], 'unsigned')
        _, optional = self.prepare(mandatory=False)
        self.assertFalse(optional['mandatory'])

    def test_a_key_clients_do_not_trust_is_refused(self):
        with self.assertRaisesRegex(ValueError, 'UPDATE_KEYS'):
            prepare(self.exe, '1.2.3', self.receipt, Ed25519PrivateKey.generate())

    def test_a_binary_built_as_another_version_is_refused(self):
        for data in [b'MZno-marker', b'MZ' + update_signing.release_marker('1.2.2'),
                     FIXTURE + update_signing.release_marker('1.2.3')]:
            self.exe.write_bytes(data)
            self.approval.update(sha256=hashlib.sha256(data).hexdigest(), size=len(data))
            self.save()
            with self.assertRaisesRegex(ValueError, 'SUPERKIRO_RELEASE_VERSION'):
                self.prepare()

    def test_a_debug_build_is_refused(self):
        for marker in update_signing.DEBUG_ONLY:
            data = FIXTURE + marker
            self.exe.write_bytes(data)
            self.approval.update(sha256=hashlib.sha256(data).hexdigest(), size=len(data))
            self.save()
            with self.assertRaisesRegex(ValueError, 'debug build'):
                self.prepare()

    def test_the_debug_markers_are_what_the_client_compiles_only_in_debug(self):
        source = (Path(__file__).resolve().parents[1] / 'crates' / 'desktop-host' / 'src'
                  / 'update.rs').read_text(encoding='utf-8')
        test_key, variable = (m.decode() for m in update_signing.DEBUG_ONLY)
        self.assertIn(f'#[cfg(debug_assertions)]\nconst TEST_UPDATE_KEY: &str = "{test_key}";',
                      source.replace('\r\n', '\n'))
        self.assertIn(f'std::env::var("{variable}")', source)

    def test_an_older_version_than_the_published_one_is_refused(self):
        _, item = self.prepare()
        newer = dict(item, version='1.10', sha256='c' * 64)
        with self.assertRaisesRegex(ValueError, 'newer version'):
            merge_manifest({'releases': [newer]}, item)
        older = dict(item, version='1.2', sha256='d' * 64)
        self.assertEqual(merge_manifest({'releases': [older]}, item)['releases'], [item])

    def test_an_entry_from_before_self_update_does_not_block_a_release(self):
        # The date-numbered releases carry no update signature: no client updated to them,
        # so a release numbered lower still replaces them.
        _, item = self.prepare()
        legacy = {k: v for k, v in item.items() if k != 'updateSignature'}
        legacy.update(version='2026.09.22', sha256='e' * 64)
        self.assertEqual(merge_manifest({'releases': [legacy]}, item)['releases'], [item])

    def test_a_release_is_numbered_major_minor_patch(self):
        for good in ['0.1.1', '1.0.0', '10.20.300']:
            check_version(good)
        for bad in ['2026.09.25', '0.1', '0.1.1.1', '01.1.1', '0.1.1-rc', '', None]:
            with self.assertRaises(ValueError):
                check_version(bad)

    def test_unapproved_wrong_version_and_non_executable_fail(self):
        for key, value in [('approvedForPublication', False), ('version', '1.2.4'), ('size', 0)]:
            previous = self.approval[key]
            self.approval[key] = value
            self.save()
            with self.assertRaises(ValueError):
                self.prepare()
            self.approval[key] = previous
        self.save()
        self.exe.write_bytes(b'not an executable')
        with self.assertRaises(ValueError):
            self.prepare()

    def test_preserves_other_platform_and_refuses_version_conflict(self):
        _, item = self.prepare()
        mac = dict(platform='macos', arch='arm64', version='1.0', sha256='abc')
        self.assertEqual(merge_manifest({'releases': [mac, item]}, item)['releases'], [mac, item])
        with self.assertRaises(ValueError):
            merge_manifest({'releases': [dict(item, sha256='different')]}, item)

    def test_historical_version_cannot_be_reused_after_manifest_advances(self):
        _, item = self.prepare()
        original = f"Superkiro-1.2.3-{item['sha256']}-windows-x64.exe"
        validate_history([original], item)
        validate_history(['Superkiro-1.2.30-' + 'a' * 64 + '-windows-x64.exe'], item)
        with self.assertRaisesRegex(ValueError, 'historically'):
            validate_history([original], dict(item, sha256='b' * 64))

    def test_short_historical_filename_cannot_be_overwritten(self):
        from publish_native_windows import publish
        data, item = self.prepare()
        ssh = MagicMock()
        sftp = ssh.open_sftp.return_value.__enter__.return_value
        sftp.listdir.return_value = [item['url'].removeprefix('/downloads/')]
        transaction = SimpleNamespace(deployment_lock=lambda _: nullcontext(),
                                      run=lambda *_: '0' * 64 + '  existing.exe')
        with patch.dict('sys.modules', {'deploy.release_candidate': transaction, 'requests': MagicMock()}), \
                patch('publish_native_windows.read_manifest', return_value={'releases': []}):
            with self.assertRaisesRegex(RuntimeError, 'Immutable artifact conflict'):
                publish(ssh, data, item)
        sftp.open.assert_not_called()
        sftp.posix_rename.assert_not_called()

    def test_legacy_publisher_cannot_bypass_acceptance(self):
        from publish_downloads import publish
        with self.assertRaisesRegex(RuntimeError, 'retired'):
            publish(object())

    def test_unsafe_version_rejected(self):
        for version in ['../bad;version', 'v1.2.3', '1.2.3-rc1', '1.2.3.4.5']:
            with self.assertRaises(ValueError):
                prepare(self.exe, version, self.receipt, self.key)


if __name__ == '__main__':
    unittest.main()
