"""Exercise provenance in disposable repositories, without network/build/signing."""
import hashlib
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
from artifact_provenance import record


class ProvenanceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.repo = self.root / 'repo'
        self.repo.mkdir()
        self.git('init')
        self.git('config', 'user.name', 'Offline Test')
        self.git('config', 'user.email', 'offline@example.invalid')
        (self.repo / 'source').write_text('fixture')
        self.git('add', '.')
        self.git('commit', '-m', 'fixture')
        self.artifact = self.root / 'fixture.exe'
        self.artifact.write_bytes(b'not a real executable')

    def git(self, *args):
        return subprocess.check_output(['git', '-C', str(self.repo), *args], stderr=subprocess.DEVNULL, text=True).strip()

    def record(self):
        return record(self.repo, self.artifact, 'test-only', 'windows', 'x64')

    def test_exact_mapping_without_claiming_signature_or_acceptance(self):
        with patch.dict(os.environ, {'GITHUB_RUN_ID': '123', 'GITHUB_RUN_ATTEMPT': '2'}):
            result = self.record()
        self.assertEqual(result['source_commit'], self.git('rev-parse', 'HEAD'))
        self.assertEqual(result['sha256'], hashlib.sha256(self.artifact.read_bytes()).hexdigest())
        self.assertEqual(result['size'], self.artifact.stat().st_size)
        self.assertEqual(result['build']['GITHUB_RUN_ATTEMPT'], '2')
        self.assertEqual(result['signature_verification'], 'not_performed')
        self.assertFalse(result['publication_approved'])

    def test_tracked_changes_rejected(self):
        (self.repo / 'source').write_text('changed')
        with self.assertRaisesRegex(ValueError, 'dirty'):
            self.record()

    def test_untracked_source_rejected(self):
        (self.repo / 'new-source').write_text('new')
        with self.assertRaisesRegex(ValueError, 'dirty'):
            self.record()

    def test_staged_changes_rejected(self):
        (self.repo / 'source').write_text('staged')
        self.git('add', '.')
        with self.assertRaisesRegex(ValueError, 'dirty'):
            self.record()

    def test_empty_and_missing_artifacts_rejected(self):
        self.artifact.write_bytes(b'')
        with self.assertRaises(ValueError):
            self.record()
        self.artifact.unlink()
        with self.assertRaises(ValueError):
            self.record()
