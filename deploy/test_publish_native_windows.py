"""Offline publication regression checks; never imports the production SSH helper."""
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from publish_native_windows import prepare, merge_manifest, validate_history


class PublicationTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name)
        self.exe, self.receipt = root / 'app.exe', root / 'accepted.json'
        self.exe.write_bytes(b'MZfixture-not-executable')
        self.approval = dict(approvedForPublication=True, version='1.2.3',
                            sha256=hashlib.sha256(self.exe.read_bytes()).hexdigest(),
                            size=self.exe.stat().st_size, platform='windows', arch='x64')
        self.save()

    def save(self):
        self.receipt.write_text(json.dumps(self.approval), encoding='utf-8')

    def prepare(self):
        return prepare(self.exe, '1.2.3', self.receipt)

    def test_exact_approved_bytes_and_immutable_url(self):
        data, item = self.prepare()
        self.assertIn(item['sha256'], item['url'])
        self.exe.write_bytes(b'MZchanged-build')
        self.assertEqual(data, b'MZfixture-not-executable')
        with self.assertRaises(ValueError):
            self.prepare()

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
        original = item['url'].removeprefix('/downloads/')
        validate_history([original], item)
        validate_history(['Superkiro-1.2.30-' + 'a' * 64 + '-windows-x64.exe'], item)
        with self.assertRaisesRegex(ValueError, 'historically'):
            validate_history([original], dict(item, sha256='b' * 64))

    def test_legacy_publisher_cannot_bypass_acceptance(self):
        from publish_downloads import publish
        with self.assertRaisesRegex(RuntimeError, 'retired'):
            publish(object())

    def test_unsafe_version_rejected(self):
        with self.assertRaises(ValueError):
            prepare(self.exe, '../bad;version', self.receipt)


if __name__ == '__main__':
    unittest.main()
