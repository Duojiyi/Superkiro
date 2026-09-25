"""Offline tests only: no SSH connections or HTTP requests are made."""
import copy
import unittest
from unittest.mock import MagicMock

from deploy import publish_model_display as script
from deploy.release_candidate import PreconditionFailed


def mapping(model, **overrides):
    return {'id': 'map-' + model, 'group_id': 'group-pro-plus', 'exposed_model_id': model,
            'target_provider_id': 'provider', 'target_model': model,
            'context_window': 200000, 'max_output': 64000, 'supports_tools': True,
            'supports_vision': True, 'supports_reasoning': True, 'credit_multiplier': 1.0,
            'visible': True, 'sort_order': 0, 'aliases': [], 'fallback_chain': [],
            'display_name': None, 'description': None, 'rate_multiplier': None, **overrides}


def config(models, revision='old'):
    return {'revision': revision, 'models': models, 'groups': ['g'], 'versions': ['v']}


class DisplayRateTests(unittest.TestCase):
    def test_only_opus_and_sonnet_multipliers_change(self):
        models = [mapping('claude-opus-4-8'), mapping('claude-sonnet-5'),
                  mapping('claude-opus-5', credit_multiplier=1.5),
                  mapping('claude-haiku-4-5'), mapping('deepseek-chat', rate_multiplier=0.25)]
        before = copy.deepcopy(models)
        result = script.with_display_rates(models)
        self.assertEqual([m['rate_multiplier'] for m in result], [2.2, 1.3, 2.2, None, 0.25])
        for old, new in zip(before, result):
            # Charging stays exactly as configured: only the displayed multiplier moves.
            self.assertEqual({k: v for k, v in new.items() if k != 'rate_multiplier'},
                             {k: v for k, v in old.items() if k != 'rate_multiplier'})
        self.assertEqual(models, before)

    def test_nothing_to_change_publishes_nothing(self):
        models = [mapping('claude-opus-4-8', rate_multiplier=2.2), mapping('glm-5')]
        self.assertIsNone(script.with_display_rates(models))

    def test_a_server_without_the_field_is_refused(self):
        legacy = mapping('claude-opus-4-8')
        del legacy['rate_multiplier']
        with self.assertRaises(PreconditionFailed):
            script.with_display_rates([legacy])


class PublishTests(unittest.TestCase):
    def test_publishes_only_models_revision_checked_and_reads_back(self):
        models = [mapping('claude-sonnet-4-6'), mapping('glm-5')]
        intended = script.with_display_rates(copy.deepcopy(models))
        calls = []

        def call(path, body=None):
            calls.append((path, body))
            if body is not None:
                return {'config': config(intended, 'new')}
            return {'config': config(models if len(calls) == 1 else intended,
                                     'old' if len(calls) == 1 else 'new')}

        record = MagicMock()
        live, published = script.publish(call, record)
        self.assertTrue(published)
        self.assertEqual(live['revision'], 'new')
        payload = calls[1][1]
        self.assertEqual(set(payload), {'expected_revision', 'reason', 'models'})
        self.assertEqual(payload['expected_revision'], 'old')
        self.assertEqual(payload['models'], intended)
        record.assert_called_once()
        self.assertEqual(record.call_args.args[1], payload)

    def test_settling_requests_are_waited_out(self):
        models = [mapping('claude-opus-4-8')]
        intended = script.with_display_rates(copy.deepcopy(models))
        answers = iter([
            {'config': config(models)},
            script.Refused('Requests are still settling; publish when idle'),
            {'config': config(models)},
            {'config': config(intended, 'new')},
            {'config': config(intended, 'new')},
        ])

        def call(path, body=None):
            answer = next(answers)
            if isinstance(answer, Exception):
                raise answer
            return answer

        wait = MagicMock()
        live, published = script.publish(call, MagicMock(), wait=wait)
        self.assertTrue(published)
        wait.assert_called_once_with(3)

    def test_any_other_refusal_stops_without_retrying(self):
        call = MagicMock(side_effect=[{'config': config([mapping('claude-opus-4-8')])},
                                      script.Refused('Configuration changed; reload')])
        with self.assertRaises(PreconditionFailed):
            script.publish(call, MagicMock(), wait=MagicMock())
        self.assertEqual(call.call_count, 2)

    def test_a_readback_that_differs_is_an_error(self):
        models = [mapping('claude-opus-4-8')]
        call = MagicMock(side_effect=[{'config': config(models)}, {'config': {}},
                                      {'config': config(models, 'new')}])
        with self.assertRaises(RuntimeError) as raised:
            script.publish(call, MagicMock())
        self.assertNotIsInstance(raised.exception, PreconditionFailed)

    def test_already_published_is_left_alone(self):
        live = config([mapping('claude-opus-4-8', rate_multiplier=2.2)])
        call = MagicMock(return_value={'config': live})
        record = MagicMock()
        self.assertEqual(script.publish(call, record), (live, False))
        call.assert_called_once_with('commercial-config')
        record.assert_not_called()


class TotpTests(unittest.TestCase):
    def test_rfc_6238_vector(self):
        # RFC 6238 appendix B, SHA-1 seed "12345678901234567890", T = 59 s: 94287082.
        self.assertEqual(script.totp_code('GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ', 59), '287082')


if __name__ == '__main__':
    unittest.main()
