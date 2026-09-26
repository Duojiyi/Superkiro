"""Offline tests only: no SSH connections or HTTP requests are made."""
import copy
import unittest
from decimal import Decimal
from unittest.mock import MagicMock

from deploy import migrate_official_pricing as script
from deploy.release_candidate import PreconditionFailed

FACE = 0.03
# The provider each model in the policy is sold through.
PROVIDERS = {'gpt-6-astra': 'kimera-direct', 'gpt-5.6-sol': 'kimera-direct',
             'gpt-5.6-terra': 'kimera-direct', 'claude-opus-5-5': 'hanyue-max'}


def mapping(model, provider='kimera-primary', **overrides):
    return {'id': 'map-' + model, 'group_id': 'group-pro-plus', 'exposed_model_id': model,
            'target_provider_id': provider, 'target_model': model, 'visible': True,
            'retired': False, 'aliases': [], 'fallback_chain': [], 'credit_multiplier': 1.0,
            **overrides}


def live_version(model, provider='kimera-primary', **overrides):
    """A price as it was published from the policy offline: 8 credits an official dollar, and
    the provider's multiplier on what it bills, in exact decimals."""
    policy = script.POLICY
    prices = policy['official_prices_per_m'][policy['priced_as'].get(model, model)]
    basis = policy['upstream_price_basis'].get(f'{provider}/{model}', prices)
    rate = policy['provider_upstream_multipliers'].get(provider, policy['upstream_multiplier'])
    version = {'id': 'official024-' + model, 'rate_card_id': 'default', 'model': model,
               'currency': 'CNY', 'pricing_mode': 'fixed', 'margin_multiplier': 1.0,
               'per_call_credit': 0, 'effective_from_secs': 100}
    for kind, price, cost in zip(script.PRICES, prices, basis):
        version[kind + '_price_per_m'] = float(Decimal(str(cost)) * Decimal(str(rate)))
        version['fixed_' + kind + '_credit_per_m'] = int(Decimal(str(price)) * 8 * 1_000_000)
    return {**version, **overrides}


def config(models, versions, settings=None, revision='old'):
    return {'revision': revision, 'models': models, 'versions': versions,
            'groups': [{'id': 'group-pro-plus', 'rate_card_id': 'default'},
                       {'id': 'group-other', 'rate_card_id': 'default'}],
            'settings': settings or {'credit_face_value_cny': FACE, 'usd_cny_rate': 7.25,
                                     'rate_updated_at_secs': 1}}


class VersionBuilderTests(unittest.TestCase):
    def test_every_price_published_from_the_policy_is_reproduced(self):
        for model in script.POLICY['official_prices_per_m']:
            provider = PROVIDERS.get(model, 'kimera-primary')
            with self.subTest(model=model):
                current = live_version(model, provider)
                version, problem = script.migrated_version(
                    mapping(model, provider), current, [], script.POLICY, FACE, 500)
                self.assertIsNone(problem)
                # Charging and cost stay as they are: only the ID, the time and the block are new.
                new = ('id', 'effective_from_secs', 'official')
                self.assertEqual({k: v for k, v in version.items() if k not in new},
                                 {k: v for k, v in current.items() if k not in new})
                self.assertEqual(version['id'], f'official-default-{model}-500')
                self.assertEqual(version['effective_from_secs'], 500)
                self.assertEqual(version['official']['credit_face_value_cny'], FACE)

    def test_hanyue_costs_its_measured_prices_and_sells_at_opus_5s(self):
        block = script.official_block(mapping('claude-opus-5-5', 'hanyue-max'), script.POLICY, FACE)
        self.assertEqual([block[kind + '_usd_per_m'] for kind in script.PRICES], [5, 25, 6.25, 0.5])
        self.assertEqual(block['cost_basis_usd_per_m'], [2, 25, 6.25, 0.5])
        self.assertEqual((block['price_multiplier'], block['cost_multiplier'], block['usd_cny']),
                         (0.24, 0.22, 1.0))
        self.assertEqual(script.credits(block), [40_000_000, 200_000_000, 50_000_000, 4_000_000])
        self.assertTrue(all(map(script.close, script.costs(block), [0.44, 5.5, 1.375, 0.11])))
        # Another provider's multiplier, the policy's default for a provider without one, and
        # the official prices when the upstream bills them.
        other = script.official_block(mapping('claude-opus-5', 'kimera-direct'), script.POLICY, FACE)
        unknown = script.official_block(mapping('claude-opus-5', 'unknown'), script.POLICY, FACE)
        self.assertEqual((other['cost_multiplier'], unknown['cost_multiplier']), (0.06, 0.08))
        self.assertNotIn('cost_basis_usd_per_m', other)

    def test_credits_round_half_away_from_zero_like_the_server(self):
        block = {'input_usd_per_m': 2.5, 'output_usd_per_m': 0.0, 'cache_creation_usd_per_m': 0.0,
                 'cache_read_usd_per_m': 0.0, 'price_multiplier': 1.0, 'usd_cny': 1.0,
                 'credit_face_value_cny': 1_000_000.0}
        self.assertEqual(round(2.5), 2)
        self.assertEqual(script.credits(block), [3, 0, 0, 0])

    def test_a_malformed_policy_is_refused_before_anything_changes(self):
        policy = copy.deepcopy(script.POLICY)
        policy['upstream_price_basis']['hanyue-max/claude-opus-5-5'] = 'claude-opus-5'
        with self.assertRaises(PreconditionFailed):
            script.official_block(mapping('claude-opus-5-5', 'hanyue-max'), policy, FACE)


class PublicationTests(unittest.TestCase):
    def test_each_listed_price_is_republished_once_with_the_settings_defaults(self):
        models = [mapping('claude-opus-5'), mapping('claude-sonnet-5'),
                  mapping('claude-opus-5-5', 'hanyue-max'),
                  mapping('claude-opus-4-6', visible=False), mapping('claude-opus-4-7', retired=True),
                  mapping('claude-opus-5', id='map-other', group_id='group-other')]
        versions = [live_version('claude-opus-5', id='older', effective_from_secs=50,
                                 fixed_input_credit_per_m=1),
                    live_version('claude-opus-5'), live_version('claude-sonnet-5'),
                    live_version('claude-opus-5-5', 'hanyue-max'),
                    live_version('claude-opus-4-6', fixed_input_credit_per_m=1),
                    live_version('claude-opus-4-7', fixed_input_credit_per_m=1)]
        update, problems = script.publication(config(models, versions), script.POLICY, 200, 230)
        self.assertEqual(problems, [])
        self.assertEqual((update['expected_revision'], update['reason']), ('old', script.REASON))
        self.assertEqual([v['model'] for v in update['versions']],
                         ['claude-opus-5', 'claude-sonnet-5', 'claude-opus-5-5'])
        self.assertEqual({v['effective_from_secs'] for v in update['versions']}, {230})
        self.assertEqual(update['settings'], {
            'credit_face_value_cny': FACE, 'usd_cny_rate': 7.25, 'rate_updated_at_secs': 1,
            'official_usd_cny': 1.0, 'default_price_multiplier': 0.24,
            'default_cost_multiplier': 0.08,
            'provider_cost_multipliers': {'kimera-primary': 0.08, 'kimera-direct': 0.06,
                                          'hanyue-max': 0.22}})
        # Once every price in force is official and the defaults are set, nothing is left.
        done = config(models, versions + update['versions'], update['settings'])
        self.assertEqual(script.publication(done, script.POLICY, 300, 330), (None, []))

    def test_every_listed_model_it_would_reprice_is_named(self):
        models = [mapping('claude-opus-5-5', 'hanyue-max'), mapping('claude-sonnet-5'),
                  mapping('claude-opus-5'), mapping('claude-opus-4-6'), mapping('claude-opus-4-7'),
                  mapping('glm-5'), mapping('gpt-5.6-sol', 'kimera-direct')]
        versions = [
            # Its cost still at Claude Opus 5's $5 input.
            live_version('claude-opus-5-5', 'hanyue-max', input_price_per_m=1.1),
            live_version('claude-sonnet-5', fixed_output_credit_per_m=81_000_000),
            live_version('claude-opus-5'),
            live_version('claude-opus-5', id='later', effective_from_secs=900),
            live_version('claude-opus-4-6', pricing_mode='cost_plus'),
            {**live_version('claude-opus-4-7', id='wildcard'), 'model': '*'},
            {**live_version('claude-opus-4-8', id='glm'), 'model': 'glm-5'},
            live_version('gpt-5.6-sol', 'kimera-direct'),
        ]
        update, problems = script.publication(config(models, versions), script.POLICY, 200, 230)
        self.assertEqual([problem.split(':')[0] for problem in problems],
                         ['claude-opus-5-5', 'claude-sonnet-5', 'claude-opus-5', 'claude-opus-4-6',
                          'claude-opus-4-7', 'glm-5'])
        for problem, reason in zip(problems, ['costs [1.1, 5.5, 1.375, 0.11], the policy gives [0.44',
                                              'credits [16000000, 81000000', 'a later price',
                                              'not a fixed CNY', 'charged at the price of *',
                                              'no official price']):
            self.assertIn(reason, problem)
        self.assertEqual([v['model'] for v in update['versions']], ['gpt-5.6-sol'])
        # And one with no price in force at all.
        _, problems = script.publication(config([mapping('claude-opus-5')], []), script.POLICY, 200, 230)
        self.assertEqual(problems, ['claude-opus-5: no price in force'])


class MigrateTests(unittest.TestCase):
    before = config([mapping('claude-opus-5')], [live_version('claude-opus-5')])

    def test_publishes_revision_checked_reads_back_and_waits_for_activation(self):
        update, _ = script.publication(self.before, script.POLICY, 1000, 1030)
        after = {**copy.deepcopy(self.before), 'revision': 'new',
                 'settings': {**update['settings'], 'rate_updated_at_secs': 1001},
                 'versions': self.before['versions'] + update['versions']}
        replies = iter([({'config': self.before}, 1000), ({'config': after}, 1005),
                        ({'config': after}, 1031)])
        sent = []

        def call(path, body=None):
            if body is not None:
                sent.append((path, body))
                return {'success': True}, 1001
            return next(replies)

        record, wait = MagicMock(), MagicMock()
        summary = script.migrate(call, record, wait=wait)
        self.assertEqual(sent, [('commercial-config', update)])
        record.assert_called_once_with(self.before, update, [])
        wait.assert_called_once_with(26)
        self.assertEqual(summary, {'status': 'active', 'effective_from_secs': 1030,
                                   'revision': 'new', 'models': ['claude-opus-5']})

    def test_a_dry_run_records_the_publication_and_sends_nothing(self):
        call, record = MagicMock(return_value=({'config': self.before}, 1000)), MagicMock()
        summary = script.migrate(call, record, dry_run=True)
        call.assert_called_once_with('commercial-config')
        _, update, problems = record.call_args.args
        self.assertEqual(([v['model'] for v in update['versions']], problems), (['claude-opus-5'], []))
        self.assertEqual(summary['status'], 'dry-run')

    def test_a_price_the_policy_would_change_stops_everything(self):
        before = config([mapping('claude-sonnet-5')],
                        [live_version('claude-sonnet-5', fixed_output_credit_per_m=81_000_000)])
        call = MagicMock(return_value=({'config': before}, 1000))
        with self.assertRaisesRegex(PreconditionFailed, 'claude-sonnet-5: credits'):
            script.migrate(call, MagicMock())
        call.assert_called_once_with('commercial-config')

    def test_a_readback_without_the_block_is_an_error(self):
        # What a server that does not know official prices would keep.
        update, _ = script.publication(self.before, script.POLICY, 1000, 1030)
        dropped = {**copy.deepcopy(self.before), 'versions': self.before['versions'] + [
            {k: v for k, v in version.items() if k != 'official'} for version in update['versions']]}
        replies = iter([({'config': self.before}, 1000), ({'config': dropped}, 1005)])

        def call(path, body=None):
            return ({'success': True}, 1001) if body is not None else next(replies)

        with self.assertRaises(RuntimeError) as raised:
            script.migrate(call, MagicMock(), wait=MagicMock())
        self.assertNotIsInstance(raised.exception, PreconditionFailed)


if __name__ == '__main__':
    unittest.main()
