"""Offline macOS publication gates. No SSH, extraction, or execution."""
import hashlib
import io
import json
from pathlib import Path
import plistlib
import struct
import tarfile
import tempfile
import unittest
from unittest.mock import patch
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from publish_native_macos import prepare, validate_archive
from publish_native_windows import validate_history
import update_signing


def bundle(arch='arm64', extra=None, executable=True, version='1.2.3'):
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode='w:gz') as archive:
        for name, data in [
            ('Superkiro.app/Contents/MacOS/Superkiro', struct.pack('<III', 0xFEEDFACF, 0x0100000C if arch == 'arm64' else 0x01000007, 0)
             + update_signing.release_marker(version)),
            ('Superkiro.app/Contents/Info.plist', plistlib.dumps(dict(CFBundleIdentifier='app.superkiro.desktop', CFBundleExecutable='Superkiro'))),
        ]:
            item = tarfile.TarInfo(name)
            item.mode = 0o755 if executable else 0o644
            item.size = len(data)
            archive.addfile(item, io.BytesIO(data))
        if extra:
            archive.addfile(extra)
    return output.getvalue()


class MacPublicationTests(unittest.TestCase):
    def setUp(self):
        self.key = Ed25519PrivateKey.generate()
        trusted = patch('update_signing.client_keys',
                        return_value=[update_signing.public_hex(self.key)])
        trusted.start()
        self.addCleanup(trusted.stop)

    def test_both_architectures_and_cross_target_rejection(self):
        for arch, wrong in [('arm64', 'x64'), ('x64', 'arm64')]:
            validate_archive(bundle(arch), arch)
            with self.assertRaises(ValueError):
                validate_archive(bundle(arch), wrong)
        with self.assertRaises(ValueError):
            validate_archive(bundle(executable=False), 'arm64')

    def test_unsafe_entries_and_duplicates_rejected(self):
        for name, kind in [('../escape', tarfile.REGTYPE), ('/absolute', tarfile.REGTYPE),
                           ('Superkiro.app/link', tarfile.SYMTYPE),
                           ('Superkiro.app/device', tarfile.CHRTYPE),
                           ('Superkiro.app/Contents/Info.plist', tarfile.REGTYPE)]:
            item = tarfile.TarInfo(name)
            item.type = kind
            item.linkname = '/outside'
            with self.assertRaises(ValueError):
                validate_archive(bundle(extra=item), 'arm64')

    def test_exact_bytes_approval_and_history(self):
        with tempfile.TemporaryDirectory() as directory:
            source, receipt = Path(directory) / 'app.tar.gz', Path(directory) / 'receipt.json'
            data = bundle()
            source.write_bytes(data)
            approval = dict(approvedForPublication=True, version='1.2.3', platform='macos', arch='arm64',
                            sha256=hashlib.sha256(data).hexdigest(), size=len(data))
            receipt.write_text(json.dumps(approval))
            approved, item = prepare(source, '1.2.3', receipt, 'arm64', self.key)
            self.assertEqual(approved, data)
            self.assertEqual(item['signature'], 'unsigned')
            self.assertTrue(item['mandatory'])
            self.assertTrue(update_signing.verify(item, item['updateSignature'],
                                                  [update_signing.public_hex(self.key)]))
            self.assertEqual(item['url'], '/downloads/Superkiro-1.2.3-Mac-ARM64.app.tar.gz')
            name = f"Superkiro-1.2.3-{item['sha256']}-macos-arm64.app.tar.gz"
            validate_history([name], item)
            with self.assertRaises(ValueError):
                validate_history([name], dict(item, sha256='0' * 64))
            for key, value in [('approvedForPublication', False), ('arch', 'x64'), ('sha256', '0' * 64), ('size', 0), ('version', 'other')]:
                receipt.write_text(json.dumps(dict(approval, **{key: value})))
                with self.assertRaises(ValueError):
                    prepare(source, '1.2.3', receipt, 'arm64', self.key)
            with self.assertRaises(ValueError):
                prepare(source, '../unsafe', receipt, 'arm64', self.key)
            # An app built as another version would install itself again at every start.
            other = bundle(version='1.2.2')
            source.write_bytes(other)
            receipt.write_text(json.dumps(dict(approval, sha256=hashlib.sha256(other).hexdigest(), size=len(other))))
            with self.assertRaisesRegex(ValueError, 'SUPERKIRO_RELEASE_VERSION'):
                prepare(source, '1.2.3', receipt, 'arm64', self.key)


if __name__ == '__main__':
    unittest.main()
