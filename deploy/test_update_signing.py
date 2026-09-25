"""Offline tests of the update signing tool; no key file outside a temporary directory."""
from pathlib import Path
import re
import tempfile
import unittest

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

from deploy import update_signing as signing

ITEM = {'platform': 'windows', 'arch': 'x64', 'version': '2026.09.25',
        'sha256': 'a' * 64, 'size': 13156864, 'url': '/downloads/x.exe'}
# The same bytes the client checks (crates/desktop-host/src/update.rs, signed_message).
EXPECTED = (b'superkiro-update/1\nplatform=windows\narch=x64\nversion=2026.09.25\n'
            b'sha256=' + b'a' * 64 + b'\nsize=13156864')


class SigningTests(unittest.TestCase):
    def test_message_matches_the_client_byte_for_byte(self):
        self.assertEqual(signing.message(ITEM), EXPECTED)
        source = Path(signing.CLIENT_KEYS).read_text(encoding='utf-8')
        self.assertIn('"superkiro-update/1\\nplatform={platform}\\narch={arch}\\nversion={version}'
                      '\\nsha256={sha256}\\nsize={size}"', source)

    def test_only_valid_entries_are_signed(self):
        for change in [{'platform': 'linux'}, {'arch': 'arm64'}, {'version': '2026.09.25-rc'},
                       {'version': '1.2.3.4.5'}, {'sha256': 'A' * 64}, {'sha256': 'a' * 63},
                       {'size': 0}, {'size': '1'}, {'size': True}]:
            with self.assertRaises(ValueError, msg=change):
                signing.message(dict(ITEM, **change))

    def test_signatures_verify_and_cover_every_signed_field(self):
        key = Ed25519PrivateKey.generate()
        public = signing.public_hex(key)
        signature = signing.sign(ITEM, key)
        self.assertTrue(signing.verify(ITEM, signature, [public]))
        self.assertTrue(signing.verify(dict(ITEM, url='/downloads/other.exe'), signature, [public]))
        for change in [{'version': '2026.09.26'}, {'sha256': 'b' * 64}, {'size': 1}]:
            self.assertFalse(signing.verify(dict(ITEM, **change), signature, [public]))
        self.assertFalse(signing.verify(ITEM, signature, [signing.public_hex(Ed25519PrivateKey.generate())]))
        self.assertFalse(signing.verify(ITEM, 'not hex', [public]))

    def test_keygen_never_replaces_a_key(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'nested' / 'key.pem'
            key = signing.keygen(path)
            self.assertEqual(signing.public_hex(signing.load(path)), signing.public_hex(key))
            with self.assertRaises(FileExistsError):
                signing.keygen(path)
            self.assertEqual(signing.public_hex(signing.load(path)), signing.public_hex(key))

    def test_the_client_trusts_a_well_formed_key(self):
        keys = signing.client_keys()
        self.assertGreaterEqual(len(keys), 1)
        self.assertTrue(all(re.fullmatch(r'[0-9a-f]{64}', key) for key in keys))

    def test_release_marker(self):
        self.assertEqual(signing.release_marker('2026.09.25'), b'superkiro-release:2026.09.25;')


if __name__ == '__main__':
    unittest.main()
