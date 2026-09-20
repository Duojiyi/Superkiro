"""Temporary-directory shell integration; network and engine verifier are fixtures."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parent
BASH = ("C:/Program Files/Git/bin/bash.exe" if os.name == "nt" else shutil.which("bash"))

@unittest.skipUnless(BASH and Path(BASH).is_file(), "bash unavailable")
class ColdBackupTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.data = self.root / "data"
        self.bin = self.root / "bin"
        self.data.mkdir()
        self.bin.mkdir()
        for name, script in {
            "curl": "exit 7",
            "docker": "echo exited",
            "kiro-gateway": "exit 0",  # Not a ledger validity test.
            "python3": 'exec "' + Path(sys.executable).as_posix() + '" "$@"',
        }.items():
            target = self.bin / name
            target.write_text("#!/usr/bin/env bash\n" + script + "\n", encoding="utf-8", newline="\n")
            target.chmod(0o755)
        self.env = {key: value for key, value in os.environ.items()
                    if key not in {"DATA_FILE", "ANCHOR_FILE", "GATEWAY_CONTAINER", "GATEWAY_UID", "GATEWAY_GID"}}
        self.env = dict(self.env, PATH=str(self.bin) + os.pathsep + os.environ["PATH"],
                        DATA_DIR=self.data.as_posix(), BACKUP_DIR=(self.root / "backups").as_posix())
        self.content = b'{"authoritative":"fixture"}'
        self.generation = "billing_state.json.gen_fixture"
        (self.data / self.generation).write_bytes(self.content)
        (self.data / "billing_state.json.anchor").write_text(json.dumps({
            "generation_file": self.generation,
            "checksum": hashlib.sha256(self.content).hexdigest(),
        }), encoding="utf-8", newline="\n")

    def run_script(self, name, *args, env=None):
        mock_bin = self.bin.as_posix()
        if os.name == "nt":
            mock_bin = "/" + mock_bin[0].lower() + mock_bin[2:]
        shell_env = dict(env or self.env, TEST_BIN=mock_bin)
        # Git Bash prepends its own binaries at startup. Reassert the fixture
        # PATH inside Bash so curl/Docker/verifier can never resolve to real ones.
        return subprocess.run([BASH, "--noprofile", "--norc", "-c",
                               'export PATH="$TEST_BIN:$PATH"; script=$1; shift; source "$script"',
                               "fixture", (SCRIPTS / name).as_posix(), *args],
                              env=shell_env, capture_output=True, text=True, timeout=30)

    def test_committed_generation_accepts_missing_or_stale_mirror(self):
        for mirror in (None, b"stale mirror"):
            if mirror is not None:
                (self.data / "billing_state.json").write_bytes(mirror)
            result = self.run_script("backup.sh")
            self.assertEqual(result.returncode, 0, result.stderr)
        snapshots = list((self.root / "backups").glob("billing_state_*.json"))
        snapshots = [p for p in snapshots if not p.name.endswith(".manifest.json")]
        self.assertEqual(len(snapshots), 2)
        self.assertTrue(all(p.read_bytes() == self.content for p in snapshots))

    def test_invalid_generation_and_force_are_rejected(self):
        (self.data / self.generation).write_bytes(b"corrupt")
        self.assertNotEqual(self.run_script("backup.sh").returncode, 0)
        self.assertFalse(list((self.root / "backups").glob("*.manifest.json")))
        self.assertNotEqual(self.run_script("restore.sh", "--force", "unused").returncode, 0)

    def mock(self, name, script):
        path = self.bin / name
        path.write_text("#!/usr/bin/env bash\n" + script + "\n", encoding="utf-8", newline="\n")
        path.chmod(0o755)

    def restore_fixture(self):
        # No real engine, Docker, ownership changes, or live data in these tests.
        bundle = self.root / "bundle"
        bundle.mkdir()
        snapshot = bundle / "snapshot.json"
        snapshot.write_bytes(self.content)
        (bundle / self.generation).write_bytes(self.content)
        (bundle / "snapshot.json.anchor").write_bytes(
            (self.data / "billing_state.json.anchor").read_bytes())
        (bundle / "snapshot.json.sha256").write_text(
            hashlib.sha256(self.content).hexdigest() + "  snapshot.json\n", encoding="utf-8", newline="\n")
        (bundle / "snapshot.manifest.json").write_text('{"status":"completed"}', encoding="utf-8", newline="\n")
        self.target = self.root / "restore-target"
        self.target.mkdir()
        self.log = self.root / "operations.log"
        self.restore_env = dict(self.env, DATA_FILE=(self.target / "custom.json").as_posix(),
                                GATEWAY_UID="1000", GATEWAY_GID="1000", OP_LOG=self.log.as_posix())
        self.snapshot = snapshot
        self.mock("id", "echo 1000")
        self.mock("chown", "exit 0")
        self.mock("sync", "exit 0")
        self.mock("cp", r'''src="${@: -2:1}"; dst="${@: -1}"
printf 'cp %s %s\n' "$src" "$dst" >> "$OP_LOG"
if [[ "${FAIL_STAGE:-}" == 1 && "$dst" == *.anchor.restore.tmp.* ]]; then
  /usr/bin/cp "$@" || exit
  exit 20
fi
if [[ "${FAIL_PREP:-}" == 1 && "$dst" == */.rollback_*/* && "$src" == */old.gen ]]; then exit 21; fi
if [[ "${FAIL_ROLLBACK:-}" == 1 && "$src" == */.rollback_*/old.gen ]]; then exit 22; fi
exec /usr/bin/cp "$@"''')
        self.mock("mv", r'''src="${@: -2:1}"; dst="${@: -1}"
printf 'mv %s %s\n' "$src" "$dst" >> "$OP_LOG"
if [[ "${FAIL_PUBLISH:-}" == 1 && "$src" == *.anchor.restore.tmp.* ]]; then
  if [[ "${FAIL_AFTER_MOVE:-}" == 1 ]]; then /usr/bin/mv "$@" || exit; fi
  exit 23
fi
exec /usr/bin/mv "$@"''')
        self.mock("rm", r'''printf 'rm %s\n' "$*" >> "$OP_LOG"
if [[ "${FAIL_REMOVE_ANCHOR:-}" == 1 && "${@: -1}" == */custom.json.anchor ]]; then exit 24; fi
exec /usr/bin/rm "$@"''')

    def old_state(self, mirror=True, incoming=False):
        files = {"old.gen": b"old authority",
                 "custom.json.anchor": b'{"generation_file":"old.gen"}'}
        if mirror:
            files["custom.json"] = b"old custom mirror"
        if incoming:
            files[self.generation] = b"preexisting incoming generation"
        for name, content in files.items():
            (self.target / name).write_bytes(content)
        return files

    def restore(self, **overrides):
        result = self.run_script("restore.sh", self.snapshot.as_posix(),
                                 env=dict(self.restore_env, **overrides))
        return result

    def assert_live_files(self, expected):
        actual = {p.name: p.read_bytes() for p in self.target.iterdir() if p.is_file()}
        self.assertEqual(actual, expected)
        self.assertFalse(list(self.target.glob("*.tmp.*")))

    def test_custom_basename_and_preexisting_generation_rollback(self):
        self.restore_fixture()
        expected = self.old_state(incoming=True)
        result = self.restore(FAIL_PUBLISH="1", FAIL_AFTER_MOVE="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("successfully restored", result.stderr)
        self.assert_live_files(expected)
        moves = [line for line in self.log.read_text().splitlines()
                 if line.startswith("mv ") and ".rollback.tmp." in line]
        self.assertEqual([line.rsplit("/", 1)[-1] for line in moves],
                         ["old.gen", self.generation, "custom.json", "custom.json.anchor"])

    def test_generation_preparation_failure_aborts_before_publication(self):
        self.restore_fixture()
        expected = self.old_state(mirror=False)
        result = self.restore(FAIL_PREP="1")
        self.assertNotEqual(result.returncode, 0)
        self.assert_live_files(expected)
        self.assertNotIn("mv ", self.log.read_text())
        self.assertNotIn("successfully", result.stdout + result.stderr)

    def test_rollback_failure_accumulates_and_keeps_incoming_authority(self):
        self.restore_fixture()
        self.old_state()
        result = self.restore(FAIL_PUBLISH="1", FAIL_AFTER_MOVE="1", FAIL_ROLLBACK="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("rollback incomplete", result.stderr)
        self.assertNotIn("successfully restored", result.stderr)
        # Failure in the first rollback copy must not suppress later attempts.
        self.assertEqual((self.target / "custom.json").read_bytes(), b"old custom mirror")
        anchor = json.loads((self.target / "custom.json.anchor").read_text())
        self.assertEqual(anchor["generation_file"], self.generation)
        self.assertEqual((self.target / self.generation).read_bytes(), self.content)

    def test_new_target_failure_cleans_partial_publication(self):
        self.restore_fixture()
        self.target.rmdir()  # Exercise a not-yet-created destination as well.
        for after_move in ("0", "1"):
            with self.subTest(after_move=after_move):
                result = self.restore(FAIL_PUBLISH="1", FAIL_AFTER_MOVE=after_move)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("successfully restored", result.stderr)
                self.assert_live_files({})

    def test_missing_mirror_rollback_restores_absence_before_generation_cleanup(self):
        self.restore_fixture()
        expected = self.old_state(mirror=False)
        result = self.restore(FAIL_PUBLISH="1", FAIL_AFTER_MOVE="1")
        self.assertNotEqual(result.returncode, 0)
        self.assert_live_files(expected)
        lines = self.log.read_text().splitlines()
        anchor_restore = next(i for i, line in enumerate(lines)
                              if line.startswith("mv ") and ".anchor.rollback.tmp." in line)
        generation_delete = next(i for i, line in enumerate(lines)
                                 if line.startswith("rm ") and line.endswith("/" + self.generation))
        self.assertLess(anchor_restore, generation_delete)

    def test_staging_failure_cleans_temporary_files_without_publication(self):
        self.restore_fixture()
        expected = self.old_state()
        result = self.restore(FAIL_STAGE="1")
        self.assertNotEqual(result.returncode, 0)
        self.assert_live_files(expected)
        self.assertNotIn("mv ", self.log.read_text())

    def test_new_target_anchor_removal_failure_preserves_generation(self):
        self.restore_fixture()
        result = self.restore(FAIL_PUBLISH="1", FAIL_AFTER_MOVE="1", FAIL_REMOVE_ANCHOR="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("rollback incomplete", result.stderr)
        self.assertNotIn("successfully restored", result.stderr)
        self.assertFalse((self.target / "custom.json").exists())
        self.assertTrue((self.target / "custom.json.anchor").is_file())
        self.assertEqual((self.target / self.generation).read_bytes(), self.content)

    def test_runtime_readability_failure_aborts_before_publication(self):
        self.restore_fixture()
        expected = self.old_state()
        self.mock("id", "echo 0")
        self.mock("setpriv", '[[ " $* " != *" test -r "* ]]')
        result = self.restore()
        self.assertNotEqual(result.returncode, 0)
        self.assert_live_files(expected)
        self.assertNotIn("mv ", self.log.read_text())

    def test_restore_http_response_vetoes_stopped_container(self):
        self.restore_fixture()
        self.mock("curl", "exit 0")
        result = self.restore()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("stop gateway", result.stderr)
        self.assertEqual(list(self.target.iterdir()), [])

    def test_restore_container_probe_fails_closed(self):
        self.restore_fixture()
        self.mock("docker", 'printf "%s\\n" "$*" >> "$OP_LOG"; '
                  'echo "${MOCK_STATE:-}"; exit "${MOCK_DOCKER_EXIT:-0}"')
        for state, code in (("", "1"), ("", "0"), ("running", "0"), ("paused", "0"),
                            ("restarting", "0"), ("dead", "0"), ("removing", "0"), ("unknown", "0")):
            with self.subTest(state=state, code=code):
                result = self.restore(MOCK_STATE=state, MOCK_DOCKER_EXIT=code)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(list(self.target.iterdir()), [])
        self.assertIn("-- kiro-gateway", self.log.read_text())
        for state in ("exited", "created"):
            with self.subTest(state=state):
                result = self.restore(MOCK_STATE=state, GATEWAY_CONTAINER="custom-gateway")
                self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("-- custom-gateway", self.log.read_text())

    @unittest.skipIf(os.name == "nt", "POSIX restore ownership requires Linux")
    def test_restore_uses_configured_runtime_identity(self):
        result = self.run_script("backup.sh")
        self.assertEqual(result.returncode, 0, result.stderr)
        manifest = next((self.root / "backups").glob("*.manifest.json"))
        restored = self.root / "restored"
        uid, gid = (1000, 1000) if os.geteuid() == 0 else (os.getuid(), os.getgid())
        self.root.chmod(0o755)
        env = dict(self.env, DATA_DIR=restored.as_posix(), GATEWAY_UID=str(uid), GATEWAY_GID=str(gid))
        result = self.run_script("restore.sh", manifest.as_posix(), env=env)
        self.assertEqual(result.returncode, 0, result.stderr)
        for name in ("billing_state.json", "billing_state.json.anchor", self.generation):
            stat = (restored / name).stat()
            self.assertEqual((stat.st_uid, stat.st_gid, stat.st_mode & 0o777), (uid, gid, 0o600))
        self.assertEqual((restored / "billing_state.json").read_bytes(), self.content)

if __name__ == "__main__":
    unittest.main()
