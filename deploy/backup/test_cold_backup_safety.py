"""Cold-backup safety integration tests. Docker and HTTP are always local stubs."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

SCRIPT = Path(__file__).with_name("backup.sh")
BASH = Path("C:/Program Files/Git/bin/bash.exe") if os.name == "nt" else Path(shutil.which("bash") or "/missing-bash")


def bash_path(path):
    value = path.as_posix()
    return "/" + value[0].lower() + value[2:] if os.name == "nt" else value


@unittest.skipUnless(BASH.is_file(), "bash unavailable")
class ColdBackupSafetyTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="cold-backup-safety-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.bin = self.root / "bin"
        self.data = self.root / "data"
        self.backups = self.root / "backups"
        for directory in (self.bin, self.data, self.backups):
            directory.mkdir()
        self.log = self.root / "docker.log"
        stubs = {
            "docker": 'printf "%s\\n" "$@" > "$DOCKER_LOG"\n'
                      'printf "%s\\n" "$FAKE_STATE"\nexit "$FAKE_DOCKER_EXIT"',
            "curl": 'exit "$FAKE_CURL_EXIT"',
            "python3": 'exec "' + Path(sys.executable).as_posix() + '" "$@"',
        }
        for name, body in stubs.items():
            path = self.bin / name
            path.write_text("#!/usr/bin/env bash\n" + body + "\n", encoding="utf-8")
            path.chmod(0o755)
        self.env = dict(os.environ)
        for name in ("DATA_FILE", "ANCHOR_FILE", "GATEWAY_CONTAINER", "BASH_ENV", "ENV"):
            self.env.pop(name, None)
        self.env.update(PATH=str(self.bin) + os.pathsep + os.environ["PATH"],
                        DATA_DIR=self.data.as_posix(), BACKUP_DIR=self.backups.as_posix(),
                        RETENTION_DAYS="7", TEST_BIN=bash_path(self.bin), DOCKER_LOG=self.log.as_posix(),
                        FAKE_STATE="exited", FAKE_DOCKER_EXIT="0", FAKE_CURL_EXIT="7",
                        DEPLOYMENT_LOCK=(self.root / "deployment.lock").as_posix())
        self.content = b'{"snapshot":"isolated test fixture"}'
        (self.data / "billing_state.json").write_bytes(self.content)
        (self.data / "billing_state.json.anchor").write_text(json.dumps({
            "checksum": hashlib.sha256(self.content).hexdigest(),
        }), encoding="utf-8")

    def run_backup(self, **env):
        return subprocess.run([str(BASH), "--noprofile", "--norc", "-c",
                               'export PATH="$TEST_BIN:$PATH"; script=$1; shift; source "$script"',
                               "cold-backup-safety", SCRIPT.as_posix()],
                              env=dict(self.env, **env), capture_output=True, text=True, timeout=30)

    def assert_refused(self, **env):
        result = self.run_backup(**env)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertEqual(list(self.backups.iterdir()), [], result.stderr)
        return result

    def test_stopped_container_allows_backup_without_host_health(self):
        for state in ("exited", "created"):
            with self.subTest(state=state):
                result = self.run_backup(FAKE_STATE=state)
                self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(list(self.backups.glob("*.manifest.json"))), 2)
        self.assertEqual(self.log.read_text().splitlines(), [
            "inspect", "--type", "container", "--format", "{{.State.Status}}", "--", "kiro-gateway"])

    def test_custom_data_file_resolves_generation_beside_snapshot(self):
        custom = self.root / "custom data"
        custom.mkdir()
        data_file = custom / "custom.json"
        generation = "custom.json.gen_fixture"
        (custom / generation).write_bytes(self.content)
        # A same-named decoy in DATA_DIR must never be selected.
        (self.data / generation).write_bytes(b"wrong directory")
        Path(str(data_file) + ".anchor").write_text(json.dumps({
            "generation_file": generation,
            "checksum": hashlib.sha256(self.content).hexdigest(),
        }), encoding="utf-8")
        # The convenience mirror is intentionally absent.
        result = self.run_backup(DATA_FILE=data_file.as_posix())
        self.assertEqual(result.returncode, 0, result.stderr)
        manifest = json.loads(next(self.backups.glob("*.manifest.json")).read_text())
        self.assertEqual((self.backups / manifest["snapshot_file"]).read_bytes(), self.content)
        self.assertEqual((self.backups / generation).read_bytes(), self.content)

    def test_running_and_unknown_states_refuse_even_when_health_is_unreachable(self):
        for state in ("running", "restarting", "paused", "removing", "dead", "", "unexpected", "exited\nrunning"):
            with self.subTest(state=state):
                self.assert_refused(FAKE_STATE=state)

    def test_probe_errors_never_mean_stopped(self):
        # Includes missing container, daemon/permission errors and unavailable CLI.
        # Even output claiming "exited" cannot override a failing command.
        for status in ("1", "125", "127"):
            with self.subTest(status=status):
                result = self.assert_refused(FAKE_DOCKER_EXIT=status)
                self.assertIn("cannot determine", result.stderr)

    def test_missing_docker_refuses(self):
        # An isolated PATH contains only dirname; no real Docker can be reached.
        isolated = self.root / "no-docker"
        isolated.mkdir()
        dirname = isolated / "dirname"
        dirname.write_text('shift; echo "${1%/*}"\n', encoding="utf-8")
        dirname.chmod(0o755)
        # The deployment lock is taken before anything else, and released on refusal.
        for tool in ("mkdir", "rmdir"):
            (isolated / tool).write_text(f'exec /usr/bin/{tool} "$@"\n', encoding="utf-8")
            (isolated / tool).chmod(0o755)
        result = subprocess.run([str(BASH), "--noprofile", "--norc", "-c",
                                 'export PATH="$TEST_BIN"; script=$1; shift; source "$script"',
                                 "cold-backup-safety", SCRIPT.as_posix()],
                                env=dict(self.env, TEST_BIN=bash_path(isolated)),
                                capture_output=True, text=True, timeout=30)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("docker is required", result.stderr)
        self.assertEqual(list(self.backups.iterdir()), [])
        self.assertFalse((self.root / "deployment.lock").exists())

    def test_responding_endpoint_vetoes_stopped_container(self):
        result = self.assert_refused(FAKE_CURL_EXIT="0")
        self.assertIn("endpoint still responds", result.stderr)

    def test_custom_container_identity_is_passed_as_one_argument(self):
        result = self.run_backup(GATEWAY_CONTAINER="custom gateway")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.log.read_text().splitlines()[-1], "custom gateway")

    def test_retention_prunes_cold_root_only_and_preserves_online_bundle(self):
        old = time.time() - 20 * 86400
        prefix = "billing_state_20000101_000000_1_1"
        cold_files = []
        for suffix in (".manifest.json", ".json", ".json.anchor", ".json.sha256"):
            path = self.backups / (prefix + suffix)
            path.write_bytes(b"expired cold fixture")
            os.utime(path, (old, old))
            cold_files.append(path)
        online = self.backups / "online-bundle"
        online.mkdir()
        for name in (prefix + ".manifest.json", "manifest.json", "billing_state.json.gen_fixture", "billing_state.json.anchor"):
            path = online / name
            path.write_bytes(b"online fixture must survive")
            os.utime(path, (old, old))
        loose = self.backups / "billing_state_uncommitted.json"
        loose.write_bytes(b"uncommitted fixture")
        os.utime(loose, (old, old))
        before = {path.name: path.read_bytes() for path in online.iterdir()}
        result = self.run_backup()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(all(not path.exists() for path in cold_files))
        self.assertEqual({path.name: path.read_bytes() for path in online.iterdir()}, before)
        self.assertTrue(loose.exists())
        self.assertEqual(len(list(self.backups.glob("*.manifest.json"))), 1)


if __name__ == "__main__":
    unittest.main()
