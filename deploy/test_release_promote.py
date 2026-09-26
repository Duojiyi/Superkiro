"""Offline regression checks for the release promotion transaction.

`promote()` is the highest-blast-radius code in this repository and had no test
coverage. These drive it against a mocked host and pin the failure behaviour of
each phase, in particular that a client-side check can never take a live release
off the air.
"""
import hashlib
import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import MagicMock, patch

from deploy import release_candidate as rc

RELEASE = '20260101T000000Z'
OLD = '/opt/kiro-byok/releases/20251231T000000Z'
DEST = f'/opt/kiro-byok/releases/{RELEASE}'
NEW_IMAGE = 'sha256:' + 'b' * 64
OLD_IMAGE = 'sha256:' + 'a' * 64


def staged_report():
    return {
        'release': RELEASE,
        'status': 'staging',
        'previous_release': OLD,
        'previous_image_id': OLD_IMAGE,
        'configuration_sha256': 'cfg-digest',
        'candidate_sha256': 'tree-digest',
    }


SEQUENCE = 1191


def fake_run(_ssh, command):
    """Answer only what promote() interrogates; everything else is a no-op."""
    if command.startswith('df --output=avail'):
        return f'{50 << 30}\n{1 << 20}'
    if 'billing_state.json.anchor' in command:
        return str(SEQUENCE)
    if command.startswith('docker logs kiro-gateway'):
        return f'Restored billing state at sequence {SEQUENCE}'
    if command.endswith('/build.exit'):
        return '0'
    if command.startswith('readlink -f'):
        return OLD
    if command.endswith('/build.image-id'):
        return NEW_IMAGE
    if f'docker image inspect kiro-byok:{RELEASE}' in command:
        return NEW_IMAGE
    if 'docker image inspect' in command:
        return OLD_IMAGE
    if command.startswith(f'docker compose -p deploy -f {DEST}') and 'config --format json' in command:
        return json.dumps({'services': {'gateway': {'image': f'kiro-byok:{RELEASE}'}}})
    if 'config --format json' in command:
        return json.dumps({'services': {'gateway': {
            'image': 'kiro-byok:20251231T000000Z',
            'environment': {'ADMIN_BROWSER_LOGIN': 'true'}}}})
    if "kiro-gateway --format '{{.Image}}'" in command:
        return OLD_IMAGE
    if '.State.ExitCode' in command:
        return '0'
    if '.State.OOMKilled' in command:
        return 'false'
    if "kiro-caddy --format '{{.State.Running}}'" in command:
        return 'true'
    return ''


class PromoteFailureBehaviour(unittest.TestCase):
    def setUp(self):
        self.patches = {name: patch.object(rc, name) for name in (
            'run', 'save_report', 'stop_services', 'wait_gateway',
            'switch_current', 'external_readiness', 'configuration_digest',
            'tree_digest')}
        self.mocks = {name: p.start() for name, p in self.patches.items()}
        self.addCleanup(lambda: [p.stop() for p in self.patches.values()])
        self.mocks['run'].side_effect = fake_run
        self.mocks['configuration_digest'].return_value = 'cfg-digest'
        self.mocks['tree_digest'].return_value = 'tree-digest'

    def commands(self):
        return [call.args[1] for call in self.mocks['run'].call_args_list]

    def test_client_side_readiness_failure_leaves_the_release_serving(self):
        """A failure at 'exposing' must not stop a healthy, live release.

        The new gateway has already passed its health check and has been writing
        to the data directory, so its data cannot be rewound. Stopping the site
        converts an unverified release into an outage.
        """
        report = staged_report()
        self.mocks['external_readiness'].side_effect = RuntimeError('operator network is down')

        with self.assertRaises(RuntimeError):
            rc.promote(None, report)

        self.assertEqual(report['status'], 'needs_attention')
        # Exactly the one legitimate stop during the 'stopping' phase, and no
        # teardown from the failure handler.
        self.assertEqual(self.mocks['stop_services'].call_count, 1)
        self.assertNotIn(OLD, [c.args[1] for c in self.mocks['switch_current'].call_args_list[1:]])

    def test_extra_readiness_failure_cannot_tear_down_a_deployed_release(self):
        """Public asset verification runs after the release is recorded deployed."""
        report = staged_report()
        boom = RuntimeError('published asset digest mismatch')

        def verify(_report):
            raise boom

        with self.assertRaises(RuntimeError) as caught:
            rc.promote(None, report, extra_readiness=verify)

        self.assertIs(caught.exception, boom)
        self.assertEqual(report['status'], 'deployed')
        self.assertEqual(self.mocks['stop_services'].call_count, 1)

    def test_failure_before_exposure_still_performs_the_full_data_rollback(self):
        """The 'backed_up' phase keeps its automatic restore."""
        report = staged_report()
        self.mocks['wait_gateway'].side_effect = [RuntimeError('candidate never became healthy'), None]

        with self.assertRaises(RuntimeError):
            rc.promote(None, report)

        self.assertEqual(report['status'], 'rolled_back')
        self.assertEqual(self.mocks['stop_services'].call_count, 2)
        restore = ' '.join(self.commands())
        self.assertIn('data.complete', restore)
        self.assertIn('failed-candidate-data', restore)
        # Kept customer requests never stay in a copy: not the backup, not the failed data,
        # and the backup does not copy them first only to delete them.
        self.assertIn('--exclude=data/request-archive -cf - data', restore)
        self.assertNotIn(f'cp -a {rc.BASE}/data ', restore)
        self.assertIn('/failed-candidate-data/request-archive', restore)
        self.assertIn('diff -qr --exclude=request-archive', restore)
        self.assertIn(OLD, [call.args[1] for call in self.mocks['switch_current'].call_args_list])

    def test_ingress_that_never_came_back_is_brought_up(self):
        """A failure between switching to the new release and starting Caddy must not
        leave the site down behind a healthy gateway."""
        report = staged_report()
        caddy_running = {'value': 'false'}

        def caddy_state(ssh, command):
            if "kiro-caddy --format '{{.State.Running}}'" in command:
                return caddy_running['value']
            if 'up -d' in command and command.rstrip().endswith('caddy'):
                if '--force-recreate' in command:
                    raise RuntimeError('Remote operation failed (exit 1); remote output withheld')
                caddy_running['value'] = 'true'
            return fake_run(ssh, command)

        self.mocks['run'].side_effect = caddy_state
        with self.assertRaises(RuntimeError):
            rc.promote(None, report)
        self.assertEqual(report['status'], 'needs_attention')
        self.assertEqual(caddy_running['value'], 'true')
        self.mocks['external_readiness'].assert_called_once()
        self.assertEqual(self.mocks['stop_services'].call_count, 1)

    def test_too_little_space_for_the_data_copy_stops_nothing(self):
        report = staged_report()

        def full_disk(ssh, command):
            if command.startswith('df --output=avail'):
                # The kept requests are not copied, so they need no room.
                self.assertIn('du -sb --exclude=request-archive', command)
                return f'{1 << 20}\n{1 << 30}'
            return fake_run(ssh, command)

        self.mocks['run'].side_effect = full_disk
        with self.assertRaises(rc.PreconditionFailed):
            rc.promote(None, report)
        self.assertEqual(report['status'], 'staging')
        self.mocks['stop_services'].assert_not_called()

    def test_a_copy_failure_after_a_clean_stop_brings_the_previous_release_back(self):
        """Nothing was changed, so the site must not stay down."""
        report = staged_report()

        def copy_fails(ssh, command):
            if command.startswith(f'tar -C {rc.BASE} '):
                raise RuntimeError('Remote operation failed (exit 1); remote output withheld')
            return fake_run(ssh, command)

        self.mocks['run'].side_effect = copy_fails
        with self.assertRaises(RuntimeError):
            rc.promote(None, report)
        self.assertEqual(report['status'], 'restarted_previous')
        self.assertEqual(self.mocks['stop_services'].call_count, 1)
        commands = self.commands()
        self.assertTrue(any(c.startswith(f'docker compose -p deploy -f {OLD}') and 'up -d' in c and 'gateway' in c
                            for c in commands))
        self.assertTrue(any('rm -rf -- /opt/kiro-byok/backups/release-' in c for c in commands))
        self.mocks['external_readiness'].assert_called_once()

    def test_an_abnormal_stop_still_leaves_the_services_down_for_review(self):
        report = staged_report()

        def killed(ssh, command):
            if '.State.OOMKilled' in command:
                return 'true'
            return fake_run(ssh, command)

        self.mocks['run'].side_effect = killed
        with self.assertRaises(RuntimeError):
            rc.promote(None, report)
        self.assertEqual(report['status'], 'needs_attention')
        self.assertEqual(self.mocks['stop_services'].call_count, 2)
        self.assertFalse(any('up -d' in c for c in self.commands()))

    def test_a_gateway_that_did_not_load_the_given_ledger_is_rolled_back(self):
        """Healthy is not enough: an empty data directory starts a healthy gateway too."""
        report = staged_report()

        def empty_ledger(ssh, command):
            if command.startswith('docker logs kiro-gateway'):
                return ''
            return fake_run(ssh, command)

        self.mocks['run'].side_effect = empty_ledger
        with self.assertRaises(RuntimeError):
            rc.promote(None, report)
        self.assertEqual(report['ledger_sequence'], SEQUENCE)
        self.assertEqual(report['status'], 'rolled_back')
        self.mocks['external_readiness'].assert_called_once()

    def test_secrets_are_backed_up_apart_from_the_ledger(self):
        report = staged_report()
        rc.promote(None, report)
        self.assertEqual(report['status'], 'deployed')
        joined = '\n'.join(self.commands())
        self.assertIn(f'cp -a /etc/kiro-byok /opt/kiro-byok/config-backups/release-{RELEASE}', joined)
        self.assertNotIn(f'backups/release-{RELEASE}/configuration', joined)

    def test_promotion_refuses_a_candidate_that_is_not_staged(self):
        for bad in ({'release': RELEASE, 'status': 'deployed'}, {'release': 'nope', 'status': 'staging'}):
            with self.assertRaises(ValueError):
                rc.promote(None, dict(bad))


class ConfigurationFromRepository(unittest.TestCase):
    """A release deploys the repository's compose and Caddy files, never the live ones."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        (self.root / 'deploy').mkdir()
        (self.root / 'deploy' / 'docker-compose.ip.yml').write_bytes(
            b'services:\r\n  gateway:\r\n    image: kiro-byok:local\r\n  caddy:\r\n    image: caddy:2\r\n')
        (self.root / 'deploy' / 'Caddyfile.ip').write_bytes(b'kiro.rent {\n    respond ok\n}\n')

    def repository(self):
        return rc.repository_configuration('kiro-byok:20260101T000000Z', root=self.root)

    def test_the_repository_files_are_deployed_with_the_candidate_image(self):
        configuration = self.repository()
        self.assertEqual(configuration['docker-compose.ip.yml'],
                         b'services:\n  gateway:\n    image: kiro-byok:20260101T000000Z\n  caddy:\n    image: caddy:2\n')
        self.assertEqual(configuration['Caddyfile.ip'], b'kiro.rent {\n    respond ok\n}\n')

    def test_a_compose_file_without_exactly_one_gateway_image_is_refused(self):
        for compose in ('services: {}\n', '    image: kiro-byok:a\n    image: kiro-byok:b\n'):
            with self.assertRaises(rc.PreconditionFailed):
                rc.with_gateway_image(compose, 'kiro-byok:new')

    def test_identical_configuration_deploys_without_review(self):
        configuration = self.repository()
        drift = rc.configuration_drift(dict(configuration), configuration)
        self.assertEqual(drift, '')
        self.assertIsNone(rc.accept_configuration(drift, RELEASE, {}, root=self.root))

    def test_a_difference_is_written_for_review_and_deployed_only_once_acknowledged(self):
        configuration = self.repository()
        live = dict(configuration, **{'Caddyfile.ip': b'kiro.rent {\n    respond live\n}\n'})
        drift = rc.configuration_drift(live, configuration)
        self.assertIn('-    respond live', drift)
        self.assertIn('+    respond ok', drift)
        self.assertNotIn('docker-compose', drift)
        digest = hashlib.sha256(drift.encode()).hexdigest()
        for credentials in ({}, {'accept_configuration': '0' * 64}):
            with self.assertRaises(rc.PreconditionFailed) as refused:
                rc.accept_configuration(drift, RELEASE, credentials, root=self.root)
            self.assertIn(digest, str(refused.exception))
            # The diff is for the operator's eyes only: it is never in the message.
            self.assertNotIn('respond', str(refused.exception))
        review = self.root / '.acceptance' / f'configuration-drift-{RELEASE}.diff'
        self.assertEqual(review.read_text(encoding='utf-8'), drift)
        self.assertEqual(rc.accept_configuration(drift, RELEASE, {'accept_configuration': digest}, root=self.root),
                         digest)

    def test_the_accepted_digest_does_not_depend_on_the_release_name(self):
        # The image line names each attempt's release; a change near it must still read the
        # same from one attempt to the next, or no reviewed digest could ever be accepted.
        def drift_for(image):
            live = {'docker-compose.ip.yml': f'services:\n  gateway:\n    image: {image}\n    restart: always\n'.encode(),
                    'Caddyfile.ip': b'kiro.rent {\n    respond ok\n}\n'}
            repository = {'docker-compose.ip.yml': f'services:\n  gateway:\n    image: {image}\n    restart: always\n    stop_grace_period: 90s\n'.encode(),
                          'Caddyfile.ip': b'kiro.rent {\n    respond ok\n}\n'}
            return rc.configuration_drift(live, repository)
        first, second = drift_for('kiro-byok:20260101T000000Z'), drift_for('kiro-byok:20260101T000512Z')
        self.assertIn('+    stop_grace_period: 90s', first)
        self.assertEqual(first, second)
        self.assertNotIn('20260101T', first)

    def test_a_caddyfile_that_does_not_load_stops_the_release_before_promotion(self):
        compose = b'  caddy:\n    image: caddy:2-alpine@sha256:' + b'c' * 64 + b'\n'
        with patch.object(rc, 'run', side_effect=RuntimeError('Remote operation failed (exit 1)')) as run:
            with self.assertRaises(rc.PreconditionFailed):
                rc.validate_caddyfile(None, DEST, compose)
        command = run.call_args.args[1]
        self.assertIn('--network none', command)
        self.assertIn('caddy validate', command)
        self.assertIn('caddy:2-alpine@sha256:' + 'c' * 64, command)


class BackupTooling(unittest.TestCase):
    def test_each_release_installs_its_backup_tools_and_enables_the_timer_with_credentials(self):
        for credentials, enabled in (('yes', True), ('', False)):
            commands = []

            def run(_ssh, command):
                commands.append(command)
                return credentials if 'admin-access.json' in command else ''

            with patch.object(rc, 'run', side_effect=run):
                outcome = rc.install_backup_tooling(None, DEST)
            joined = '\n'.join(commands)
            for name in rc.BACKUP_TOOLS:
                self.assertIn(f'{DEST}/deploy/backup/{name}', joined)
            self.assertIn('/etc/systemd/system/', joined)
            self.assertIn('systemctl daemon-reload', joined)
            self.assertEqual('enable --now superkiro-backup.timer' in joined, enabled)
            self.assertIn('enabled' if enabled else 'not enabled', outcome)

    def test_a_backup_tooling_failure_is_reported_not_raised(self):
        report = {'release': RELEASE, 'backup': f'/opt/kiro-byok/backups/release-{RELEASE}', 'previous_release': OLD}
        with patch.object(rc, 'pull_tree', return_value='copied'), \
                patch.object(rc, 'prune_releases', return_value=[]), \
                patch.object(rc, 'install_backup_tooling', side_effect=RuntimeError('no systemd')), \
                patch.object(rc, 'save_report'):
            rc.keep_off_host(None, report, {'backup_dir': self.id()})
        self.assertEqual(report['backup_tooling'], 'failed (RuntimeError: no systemd)')


class Retention(unittest.TestCase):
    def test_old_releases_go_and_the_newest_and_protected_ones_stay(self):
        names = [f'2026010{day}T000000Z' for day in range(1, 9)] + ['hand-made', 'lost+found']
        commands = []

        def run(_ssh, command):
            commands.append(command)
            return '\n'.join(names) if command.startswith('ls -1') else ''

        protected = f'{rc.BASE}/releases/20260101T000000Z'
        with patch.object(rc, 'run', side_effect=run):
            doomed = rc.prune_releases(None, {protected})
        self.assertEqual(doomed, ['20260102T000000Z', '20260103T000000Z'])
        removals = [c for c in commands if c.startswith('rm -rf')]
        self.assertEqual(len(removals), 2)
        self.assertTrue(all('hand-made' not in c and '20260101' not in c for c in removals))


class OffHostCopy(unittest.TestCase):
    def ssh(self, files):
        listing = '\n'.join(f'{hashlib.sha256(data).hexdigest()}  ./{name}' for name, data in files.items())
        sftp = MagicMock()
        sftp.get.side_effect = lambda remote, local: Path(local).write_bytes(files[remote.split('/data/', 1)[1]])
        ssh = MagicMock()
        ssh.open_sftp.return_value.__enter__.return_value = sftp
        return ssh, listing

    def test_every_file_is_copied_and_checked(self):
        files = {'billing_state.json': b'{"ciphertext":"x"}', 'billing_state.json.anchor': b'{"sequence": 4}'}
        ssh, listing = self.ssh(files)
        with tempfile.TemporaryDirectory() as local, patch.object(rc, 'run', return_value=listing):
            rc.pull_tree(ssh, '/opt/kiro-byok/backups/release-x/data', Path(local) / 'data')
            for name, data in files.items():
                self.assertEqual((Path(local) / 'data' / name).read_bytes(), data)

    def test_a_corrupted_or_escaping_copy_is_refused(self):
        ssh, listing = self.ssh({'billing_state.json': b'good'})
        with tempfile.TemporaryDirectory() as local:
            with patch.object(rc, 'run', return_value=listing.replace(listing[:64], '0' * 64)):
                with self.assertRaises(RuntimeError):
                    rc.pull_tree(ssh, '/opt/kiro-byok/backups/release-x/data', Path(local))
            with patch.object(rc, 'run', return_value=f"{'0' * 64}  ../../etc/passwd"):
                with self.assertRaises(RuntimeError):
                    rc.pull_tree(ssh, '/opt/kiro-byok/backups/release-x/data', Path(local))


class ReleaseSource(unittest.TestCase):
    """A release is built only from a clean commit on origin/main that CI passed."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name)
        self.origin, self.root = root / 'origin.git', root / 'work'
        git = lambda *args, cwd=root: subprocess.run(['git', *args], cwd=cwd, check=True, capture_output=True)
        git('init', '--bare', '-b', 'main', str(self.origin))
        git('clone', str(self.origin), str(self.root))
        for args in (['config', 'user.email', 't@example.invalid'], ['config', 'user.name', 't']):
            git(*args, cwd=self.root)
        (self.root / 'crates').mkdir()
        (self.root / 'crates' / 'lib.rs').write_text('fn main() {}\n')
        git('add', '.', cwd=self.root)
        git('commit', '-m', 'init', cwd=self.root)
        git('push', 'origin', 'main', cwd=self.root)
        self.git = lambda *args: git(*args, cwd=self.root)

    def verify(self, conclusions=('success',), allow_untested=False):
        with patch.object(rc, 'ci_conclusions', return_value=list(conclusions)):
            return rc.verified_commit(allow_untested=allow_untested, root=self.root)

    def test_a_pushed_commit_that_ci_passed_is_accepted(self):
        commit = self.verify()
        self.assertEqual(len(commit), 40)

    def test_local_edits_untracked_files_and_unpushed_commits_are_refused(self):
        (self.root / 'crates' / 'lib.rs').write_text('fn main() { broken }\n')
        with self.assertRaises(rc.PreconditionFailed):
            self.verify()
        self.git('checkout', '--', '.')
        (self.root / 'crates' / 'new.rs').write_text('')
        with self.assertRaises(rc.PreconditionFailed):
            self.verify()
        (self.root / 'crates' / 'new.rs').unlink()
        self.git('commit', '--allow-empty', '-m', 'local only')
        with self.assertRaises(rc.PreconditionFailed):
            self.verify()

    def test_failed_pending_or_missing_ci_is_refused_unless_explicitly_waived(self):
        for conclusions in ((), ('success', 'failure'), ('pending',)):
            with self.assertRaises(rc.PreconditionFailed):
                self.verify(conclusions)
        self.assertEqual(len(self.verify(('failure',), allow_untested=True)), 40)


class DeploymentLock(unittest.TestCase):
    def test_a_refusal_before_any_change_releases_the_lock_and_keeps_its_reason(self):
        with patch.object(rc, 'run') as run:
            with self.assertRaises(rc.PreconditionFailed) as caught:
                with rc.deployment_lock(None):
                    raise rc.PreconditionFailed('Current release changed; restage candidate')
            self.assertIn('restage', str(caught.exception))
            self.assertTrue(any('rmdir' in c.args[1] for c in run.call_args_list))

    def test_a_failure_keeps_the_lock_but_says_why(self):
        with patch.object(rc, 'run'):
            with self.assertRaises(RuntimeError) as caught:
                with rc.deployment_lock(None):
                    raise RuntimeError('remote state is uncertain')
            self.assertIn('remote state is uncertain', str(caught.exception))

    def test_lock_is_retained_on_failure_and_released_on_success(self):
        with patch.object(rc, 'run') as run:
            with rc.deployment_lock(None):
                pass
            self.assertTrue(any('rmdir' in c.args[1] for c in run.call_args_list))

        with patch.object(rc, 'run') as run:
            with self.assertRaises(RuntimeError):
                with rc.deployment_lock(None):
                    raise RuntimeError('remote state is uncertain')
            self.assertFalse(any('rmdir' in c.args[1] for c in run.call_args_list))


if __name__ == '__main__':
    unittest.main()
