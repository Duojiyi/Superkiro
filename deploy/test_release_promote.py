"""Offline regression checks for the release promotion transaction.

`promote()` is the highest-blast-radius code in this repository and had no test
coverage. These drive it against a mocked host and pin the failure behaviour of
each phase, in particular that a client-side check can never take a live release
off the air.
"""
import json
import unittest
from unittest.mock import patch

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


def fake_run(_ssh, command):
    """Answer only what promote() interrogates; everything else is a no-op."""
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
        self.assertIn(OLD, [call.args[1] for call in self.mocks['switch_current'].call_args_list])

    def test_promotion_refuses_a_candidate_that_is_not_staged(self):
        for bad in ({'release': RELEASE, 'status': 'deployed'}, {'release': 'nope', 'status': 'staging'}):
            with self.assertRaises(ValueError):
                rc.promote(None, dict(bad))


class DeploymentLock(unittest.TestCase):
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
