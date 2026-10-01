"""Offline packed-release checks: no process launch, SSH or publication."""
import hashlib
import json
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from publish_native_windows import prepare
import packed_windows
import update_signing


class PackedPublicationTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name)
        self.original, self.packed = root / 'Superkiro.exe', root / 'packed.exe'
        self.provenance, self.receipt = root / 'ci.json', root / 'accepted.json'
        self.original.write_bytes(b'MZoriginal' + update_signing.release_marker('1.2.3'))
        data = bytearray(160)
        data[:2] = b'MZ'
        struct.pack_into('<I', data, 0x3c, 64)
        data[64:68] = b'PE\0\0'
        struct.pack_into('<H', data, 68, 0x8664)
        struct.pack_into('<H', data, 88, 0x20b)
        self.packed.write_bytes(data)
        self.meta = dict(source_commit='a' * 40, source_dirty=False, platform='windows', arch='x64',
                         artifact='Superkiro.exe', build={'GITHUB_REPOSITORY': 'Duojiyi/Superkiro', 'GITHUB_RUN_ID': '123'})
        self.approval = dict(approvedForPublication=True, approvalBasis='user-directed-beta-publication',
                             runtimeAcceptance=False, sourceCommit='a' * 40, buildRun='123',
                             version='1.2.3', platform='windows', arch='x64')
        self.key = Ed25519PrivateKey.generate()
        trusted = patch('update_signing.client_keys', return_value=[update_signing.public_hex(self.key)])
        trusted.start()
        self.addCleanup(trusted.stop)
        resources = patch('packed_windows.check_versions')
        self.resources = resources.start()
        self.addCleanup(resources.stop)
        self.bind()

    def bind(self):
        original, packed = self.original.read_bytes(), self.packed.read_bytes()
        digest = lambda b: hashlib.sha256(b).hexdigest()
        self.meta.update(sha256=digest(original), size=len(original))
        self.provenance.write_text(json.dumps(self.meta), encoding='utf-8')
        self.approval.update(sha256=digest(packed), size=len(packed), packing=dict(
            approvedForPackedPublication=True, originalSha256=digest(original), originalSize=len(original),
            provenanceSha256=digest(self.provenance.read_bytes()), packedSha256=digest(packed), packedSize=len(packed)))
        self.save()

    def save(self):
        self.receipt.write_text(json.dumps(self.approval), encoding='utf-8')

    def prepare(self):
        return prepare(self.packed, '1.2.3', self.receipt, self.key, mandatory=False,
                       unpacked_source=self.original, unpacked_provenance=self.provenance)

    def test_packed_bytes_are_signed_and_disclosed_not_original(self):
        data, item = self.prepare()
        self.assertEqual(data, self.packed.read_bytes())
        self.assertEqual(item['packaging'], 'user-supplied-packed')
        self.assertNotEqual(item['sha256'], item['originalSha256'])
        self.assertFalse(item['mandatory'])
        self.assertFalse(item['runtimeAcceptance'])
        self.assertTrue(update_signing.verify(item, item['updateSignature'], [update_signing.public_hex(self.key)]))
        self.resources.assert_called_once_with(data, '1.2.3')

    def test_explicit_lineage_and_approval_are_required(self):
        with self.assertRaises(ValueError):
            prepare(self.packed, '1.2.3', self.receipt, self.key)
        for field in self.approval['packing'].copy():
            before = self.approval['packing'].pop(field)
            self.save()
            with self.subTest(field=field), self.assertRaises(ValueError):
                self.prepare()
            self.approval['packing'][field] = before

    def test_any_byte_change_is_refused(self):
        for path in [self.original, self.packed, self.provenance]:
            before = path.read_bytes()
            path.write_bytes(before + b' ')
            with self.subTest(path=path.name), self.assertRaises(ValueError):
                self.prepare()
            path.write_bytes(before)

    def test_wrong_ci_identity_is_refused(self):
        for field, value in [('source_dirty', True), ('source_commit', 'b' * 40), ('arch', 'arm64')]:
            before = self.meta[field]
            self.meta[field] = value
            self.bind()
            with self.subTest(field=field), self.assertRaises(ValueError):
                self.prepare()
            self.meta[field] = before

    def test_source_release_and_debug_gates_still_apply(self):
        for data in [b'MZwithout-release', b'MZ' + update_signing.release_marker('1.2.2'),
                     b'MZ' + update_signing.release_marker('1.2.3') + update_signing.DEBUG_ONLY[0]]:
            self.original.write_bytes(data)
            self.bind()
            with self.subTest(data=data), self.assertRaises(ValueError):
                self.prepare()

    def test_packed_version_or_architecture_mismatch_is_refused(self):
        self.resources.side_effect = ValueError('version mismatch')
        with self.assertRaises(ValueError):
            self.prepare()
        self.resources.side_effect = None
        data = bytearray(self.packed.read_bytes())
        struct.pack_into('<H', data, 68, 0x14c)
        self.packed.write_bytes(data)
        self.bind()
        with self.assertRaises(ValueError):
            self.prepare()

    def test_truncated_pe_is_refused(self):
        for data in [b'', b'MZ', b'MZ' + b'\xff' * 100]:
            with self.subTest(size=len(data)), self.assertRaises(ValueError):
                packed_windows.check_pe(data)
