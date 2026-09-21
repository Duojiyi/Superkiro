"""Offline tests only: no SSH connections or HTTP requests are made."""
import copy
import io
import json
import unittest
from contextlib import redirect_stdout
from unittest.mock import MagicMock, patch

from deploy import publish_verified_models as script


def capabilities():
    return {'verified': True, 'context_window': 1000000, 'max_output': 64000,
            'supports_tools': True, 'supports_vision': True, 'supports_reasoning': True}


def mapping(model, **overrides):
    return {'id': 'manual-' + model, 'group_id': script.GROUP,
            'exposed_model_id': model, 'target_provider_id': 'manual-provider',
            'target_model': 'manual-upstream', **capabilities(),
            'credit_multiplier': 2.5, 'visible': False, 'sort_order': 42,
            'aliases': ['manual-alias-' + model],
            'fallback_chain': [{'provider_id': 'backup', 'target_model': 'backup-model'}],
            **overrides}


class PublishTests(unittest.TestCase):
    def test_existing_capabilities_and_all_other_fields_unchanged(self):
        existing = [mapping(m, context_window=300000 + i, max_output=16000 + i)
                    for i, m in enumerate(script.MODELS)]
        before = copy.deepcopy(existing)
        # Even conflicting supplied capabilities cannot replace manual values.
        result = script.merge_models(existing, {m: capabilities() for m in script.MODELS})
        self.assertEqual(result, before)
        self.assertEqual(existing, before)

    def test_addition_preserves_non_targets_other_groups_and_config(self):
        existing = [mapping('unrelated'), mapping(script.MODELS[0], group_id='other')]
        config = {'revision': 'old', 'models': existing, 'settings': {'custom': True},
                  'groups': ['manual-group'], 'rate_cards': ['manual-rate'], 'versions': []}
        before = copy.deepcopy(config)
        req = MagicMock(side_effect=[{'config': config}, {'config': {'revision': 'new'}}])
        script.publish(req, {m: capabilities() for m in script.MODELS})
        payload = req.call_args.args[1]
        self.assertEqual(payload['models'][:2], existing)
        self.assertEqual(len(payload['models']), 2 + len(script.MODELS))
        for new in payload['models'][2:]:
            for key in script.CAPABILITIES:
                self.assertEqual(new[key], capabilities()[key])
        self.assertEqual(set(payload), {'models', 'expected_revision', 'reason'})
        self.assertEqual(payload['expected_revision'], 'old')
        self.assertEqual(config, before)

    def test_missing_last_new_capability_fails_before_any_write(self):
        req = MagicMock(return_value={'config': {'revision': 'old', 'models': []}})
        verified = {m: capabilities() for m in script.MODELS[:-1]}
        with self.assertRaises(ValueError):
            script.publish(req, verified)
        req.assert_called_once_with('commercial-config')

    def test_invalid_or_unverified_capabilities_fail_before_any_write(self):
        invalid = [None, {}, {**capabilities(), 'verified': False},
                   {**capabilities(), 'max_output': True},
                   {**capabilities(), 'context_window': 0},
                   {**capabilities(), 'max_output': 1000001},
                   {**capabilities(), 'context_window': 10000001},
                   {**capabilities(), 'supports_vision': 'false'}]
        for field in script.CAPABILITIES:
            caps = capabilities()
            del caps[field]
            invalid.append(caps)
        for caps in invalid:
            with self.subTest(caps=caps):
                req = MagicMock(return_value={'config': {'revision': 'old', 'models': []}})
                with self.assertRaises(ValueError):
                    script.publish(req, {script.MODELS[0]: caps})
                req.assert_called_once_with('commercial-config')

    def test_alias_does_not_supply_new_model_capabilities(self):
        existing = [mapping('different-model', aliases=[script.MODELS[0]])]
        with self.assertRaises(ValueError):
            script.merge_models(existing, {'different-model': capabilities()})

    def test_mapping_id_collision_fails(self):
        existing = [mapping('unrelated', id='kimera-test-' + script.MODELS[0])]
        with self.assertRaises(ValueError):
            script.merge_models(existing, {m: capabilities() for m in script.MODELS})

    def test_existing_models_need_no_input_or_write(self):
        config = {'revision': 'old', 'models': [mapping(m) for m in script.MODELS]}
        req = MagicMock(return_value={'config': config})
        self.assertEqual(script.publish(req, {}), config)
        req.assert_called_once_with('commercial-config')

    def test_main_uses_cookie_session_csrf_and_never_logs_secrets(self):
        probes = json.dumps([{'model': m, 'status': 200, 'has_text': True, 'tool_use': True}
                            for m in script.MODELS])
        stdin = io.StringIO(json.dumps({'password': 'ssh-secret', 'verified_capabilities':
                                       {m: capabilities() for m in script.MODELS}}))
        ssh, session = MagicMock(), MagicMock()
        session.headers = {}
        ssh.open_sftp.return_value.__enter__.return_value.open.return_value.read.return_value = (
            b'{"username":"admin-user","password":"admin-secret"}')
        responses = [{}, {'csrfToken': 'csrf-secret'},
                     {'config': {'revision': 'old', 'models': []}},
                     {'config': {'revision': 'new'}}]
        calls = []

        def request(method, url, **kwargs):
            calls.append((method, url, dict(session.headers), kwargs))
            return MagicMock(status_code=200, json=MagicMock(return_value=responses.pop(0)))

        session.request.side_effect = request
        output = io.StringIO()
        with patch.object(script.sys, 'stdin', stdin), \
                patch.object(script.Path, 'read_text', return_value=probes), \
                patch.object(script.Path, 'write_text', side_effect=AssertionError('disk write')), \
                patch.object(script, 'connect', return_value=ssh) as connect, \
                patch.object(script.requests, 'Session') as factory, redirect_stdout(output):
            factory.return_value.__enter__.return_value = session
            script.main()
        connect.assert_called_once_with('ssh-secret', False)
        ssh.close.assert_called_once()
        self.assertEqual([c[0] for c in calls], ['POST', 'GET', 'GET', 'POST'])
        self.assertEqual(calls[0][3]['json'], {'username': 'admin-user', 'password': 'admin-secret'})
        self.assertEqual(calls[-1][2]['x-csrf-token'], 'csrf-secret')
        for _, url, headers, kwargs in calls:
            self.assertTrue(url.startswith('https://kiro.rent/api/v1/admin/'))
            self.assertEqual(headers['Origin'], 'https://kiro.rent')
            self.assertNotIn('Authorization', headers)
            self.assertNotIn('x-admin-key', headers)
            self.assertFalse(kwargs['allow_redirects'])
        for secret in ('ssh-secret', 'admin-user', 'admin-secret', 'csrf-secret'):
            self.assertNotIn(secret, output.getvalue())


class PricingCapabilityTests(unittest.TestCase):
    def test_valid_boundaries_preserve_model_capabilities(self):
        from deploy.publish_pricing import validate_model_limits
        for context, output in [(1, 1), (200001, 64000), (1000000, 128000),
                                (10000000, 10000000)]:
            with self.subTest(context=context, output=output):
                model = mapping('example', context_window=context, max_output=output)
                before = copy.deepcopy(model)
                validate_model_limits(model)
                self.assertEqual(model, before)

    def test_default_pricing_rejects_1m_but_capability_is_valid(self):
        from deploy import publish_pricing as pricing
        model = mapping('example', context_window=1000000, max_output=128000)
        pricing.validate_model_limits(model)
        with patch.object(pricing, 'POLICY', {}):
            with self.assertRaisesRegex(RuntimeError, 'manual rate confirmation'):
                pricing.validate_pricing_limits(model)
            pricing.validate_pricing_limits({**model, 'context_window': 200000})

    def test_explicit_per_model_policy_allows_1m_without_changing_rates(self):
        from deploy import publish_pricing as pricing
        policy = copy.deepcopy(pricing.POLICY)
        model_id = next(iter(policy['official_prices_per_m']))
        policy['verified_context_windows'] = {model_id: 1000000}
        before = copy.deepcopy(policy)
        model = mapping(model_id, context_window=1000000, max_output=128000)
        original_price = pricing.version(model_id, 'default', 100)
        with patch.object(pricing, 'POLICY', policy):
            pricing.validate_pricing_limits(model)
            self.assertEqual(pricing.version(model_id, 'default', 100), original_price)
            with self.assertRaisesRegex(RuntimeError, 'manual rate confirmation'):
                pricing.validate_pricing_limits({**model, 'context_window': 1000001})
            with self.assertRaisesRegex(RuntimeError, 'manual rate confirmation'):
                pricing.validate_pricing_limits({**model, 'exposed_model_id': 'other-model'})
        self.assertEqual(policy, before)

    def test_invalid_pricing_policy_fails_closed(self):
        from deploy import publish_pricing as pricing
        model = mapping('example', context_window=1000000, max_output=128000)
        for windows in (None, [], {'example': True}, {'example': '1000000'},
                        {'example': 0}, {'example': 10000001}):
            with self.subTest(windows=windows), \
                    patch.object(pricing, 'POLICY', {'verified_context_windows': windows}):
                with self.assertRaises(RuntimeError):
                    pricing.validate_pricing_limits(model)

    def test_invalid_boundaries_and_types_rejected(self):
        from deploy.publish_pricing import validate_model_limits
        for context, output in [(0, 1), (1, 0), (-1, 1), (1, -1), (100, 101),
                                (10000001, 1), (True, 1), (100, True),
                                ('1000000', 1), (1000000, 1.5), (None, 1), (1, None)]:
            with self.subTest(context=context, output=output):
                with self.assertRaises(RuntimeError):
                    validate_model_limits({'context_window': context, 'max_output': output})


if __name__ == '__main__':
    unittest.main()
