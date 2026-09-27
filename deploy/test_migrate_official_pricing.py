"""Offline tests only: no SSH connections or HTTP requests are made."""
import copy
import math
import unittest
from decimal import Decimal
from unittest.mock import MagicMock

from deploy import migrate_official_pricing as script
from deploy.release_candidate import PreconditionFailed

# Historical migration fixtures deliberately use the original 0.24 policy.
# Current per-model pricing is covered separately; never silently reprice this fixture.
def setUpModule():
    global original_policy
    original_policy = script.POLICY
    legacy = copy.deepcopy(original_policy)
    legacy['retail_multiplier'] = 0.24
    legacy.pop('model_retail_multipliers', None)
    script.POLICY = legacy


def tearDownModule():
    script.POLICY = original_policy


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
    """A configuration as the release that knows official prices sends it."""
    return {'revision': revision, 'models': models, 'versions': versions,
            'groups': [{'id': 'group-pro-plus', 'rate_card_id': 'default', 'margin_multiplier': 1.0},
                       {'id': 'group-other', 'rate_card_id': 'default', 'margin_multiplier': 1.0}],
            'settings': settings or {'credit_face_value_cny': FACE, 'usd_cny_rate': 7.25,
                                     'rate_updated_at_secs': 1},
            'plans': [], 'cards_by_plan': {}}


# hanyue's measured route, as the settings cost it after the migration.
HANYUE_ROUTE = {'hanyue-max/claude-opus-5-5': {'basis_usd_per_m': [2.0, 25.0, 6.25, 0.5],
                                               'cost_multiplier': 0.22}}
# Token counts seen in production, and the extremes.
TOKENS_SEEN = [(1, 0, 0, 0), (0, 1, 0, 0), (84_657, 4_094, 0, 76_000),
               (1_000_000, 1_000_000, 1_000_000, 1_000_000), (123_457, 9_999, 31_337, 7),
               (10_000_000, 3, 2_500_000, 999_999)]


def table_entry(model):
    """The official price the migration adds for an upstream model: the one it is priced from."""
    policy = script.POLICY
    usd = policy['official_prices_per_m'][policy['priced_as'].get(model, model)]
    return {**{f'{kind}_usd_per_m': float(value) for kind, value in zip(script.PRICES, usd)},
            'note': script.OFFICIAL_NOTE, 'updated_at_secs': 0}


def micro_cny(prices_per_m, tokens):
    """What the server records a request as costing, from CNY per million tokens by class, in its
    order: the tokens of each class times its price over a million, added up, then rounded to
    micro-CNY, halves away from zero, and never below zero."""
    cny = sum(count * price / 1_000_000 for count, price in zip(tokens, prices_per_m))
    return max(0, math.floor(cny * 1_000_000 + 0.5))


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
    def test_each_callable_price_is_republished_once_with_the_settings_defaults(self):
        # A hidden model can still be called by name: it is republished with the listed ones. A
        # retired one cannot, and is left alone.
        models = [mapping('claude-opus-5'), mapping('claude-sonnet-5'),
                  mapping('claude-opus-5-5', 'hanyue-max'),
                  mapping('claude-opus-4-6', visible=False), mapping('claude-opus-4-7', retired=True),
                  mapping('claude-opus-5', id='map-other', group_id='group-other')]
        versions = [live_version('claude-opus-5', id='older', effective_from_secs=50,
                                 fixed_input_credit_per_m=1),
                    live_version('claude-opus-5'), live_version('claude-sonnet-5'),
                    live_version('claude-opus-5-5', 'hanyue-max'),
                    live_version('claude-opus-4-6'),
                    live_version('claude-opus-4-7', fixed_input_credit_per_m=1)]
        update, problems = script.publication(config(models, versions), script.POLICY, 200, 230)
        self.assertEqual(problems, [])
        self.assertEqual((update['expected_revision'], update['reason']), ('old', script.REASON))
        self.assertEqual([v['model'] for v in update['versions']],
                         ['claude-opus-5', 'claude-sonnet-5', 'claude-opus-5-5', 'claude-opus-4-6'])
        self.assertEqual({v['effective_from_secs'] for v in update['versions']}, {230})
        self.assertEqual(update['settings'], {
            'credit_face_value_cny': FACE, 'usd_cny_rate': 7.25, 'rate_updated_at_secs': 1,
            'legacy_credit_face_value_cny': FACE,
            'official_usd_cny': 1.0, 'default_price_multiplier': 0.24,
            'default_cost_multiplier': 0.08,
            'provider_cost_multipliers': {'kimera-primary': 0.08, 'kimera-direct': 0.06,
                                          'hanyue-max': 0.22},
            'route_costs': HANYUE_ROUTE,
            # Every upstream model a callable mapping sends to; not the retired one's.
            'official_prices': {model: table_entry(model) for model in
                                ['claude-opus-5', 'claude-sonnet-5', 'claude-opus-5-5',
                                 'claude-opus-4-6']}})
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
        # The cost on hanyue's route is wrong too, and would move to its measured basis.
        self.assertEqual([problem.split(':')[0] for problem in problems],
                         ['claude-opus-5-5', 'claude-sonnet-5', 'claude-opus-5', 'claude-opus-4-6',
                          'claude-opus-4-7', 'glm-5',
                          'route hanyue-max/claude-opus-5-5 (claude-opus-5-5 in group-pro-plus)'])
        for problem, reason in zip(problems, ['costs [1.1, 5.5, 1.375, 0.11], the policy gives [0.44',
                                              'credits [16000000, 81000000', 'a later price',
                                              'not a fixed CNY', 'charged at the price of *',
                                              'no official price']):
            self.assertIn(reason, problem)
        self.assertEqual([v['model'] for v in update['versions']], ['gpt-5.6-sol'])
        # And one with no price in force at all.
        _, problems = script.publication(config([mapping('claude-opus-5')], []), script.POLICY, 200, 230)
        self.assertEqual(problems, [
            'claude-opus-5: no price in force',
            'route kimera-primary/claude-opus-5 (claude-opus-5 in group-pro-plus): costs an '
            'estimate today, [0.4, 2.0, 0.5, 0.04] after the official price table is filled'])

    def test_a_hidden_price_it_would_change_or_a_multiplier_not_1_is_named(self):
        models = [mapping('claude-opus-4-6', visible=False), mapping('claude-opus-5'),
                  mapping('claude-sonnet-5', group_id='group-other'),
                  mapping('claude-opus-4-8', credit_multiplier=2.0)]
        versions = [live_version('claude-opus-4-6', fixed_input_credit_per_m=1),
                    live_version('claude-opus-5'), live_version('claude-sonnet-5'),
                    live_version('claude-opus-4-8')]
        before = config(models, versions)
        before['groups'][1]['margin_multiplier'] = 1.5
        update, problems = script.publication(before, script.POLICY, 200, 230)
        self.assertEqual([problem.split(':')[0] for problem in problems],
                         ['claude-opus-4-6', 'claude-sonnet-5', 'claude-opus-4-8'])
        self.assertIn('credits [1, 200000000', problems[0])
        self.assertIn('分组倍率 1.5 and 模型倍率 1.0 must both be 1', problems[1])
        self.assertIn('分组倍率 1.0 and 模型倍率 2.0 must both be 1', problems[2])
        self.assertEqual([v['model'] for v in update['versions']], ['claude-opus-5'])
        call = MagicMock(return_value=({'config': before}, 1000))
        with self.assertRaisesRegex(PreconditionFailed, 'claude-sonnet-5: 分组倍率 1.5'):
            script.migrate(call, MagicMock())
        call.assert_called_once_with('commercial-config')


class RouteCostTests(unittest.TestCase):
    def test_a_measured_route_is_costed_exactly_as_its_versions_are_today(self):
        routes = script.route_costs(script.POLICY)
        self.assertEqual(routes, HANYUE_ROUTE)
        tokens_seen = [(1, 0, 0, 0), (0, 1, 0, 0), (84_657, 4_094, 0, 76_000),
                       (1_000_000, 1_000_000, 1_000_000, 1_000_000), (123_457, 9_999, 31_337, 7),
                       (10_000_000, 3, 2_500_000, 999_999)]
        for route, cost in routes.items():
            provider, _, model = route.partition('/')
            with self.subTest(route=route):
                # The prices per million the settings give it, as the server multiplies them.
                official = [usd * cost['cost_multiplier'] * script.OFFICIAL_USD_CNY
                            for usd in cost['basis_usd_per_m']]
                version = live_version(model, provider)
                today = [version[kind + '_price_per_m'] for kind in script.PRICES]
                # The same binary numbers, so the same cost for any request.
                self.assertEqual(official, today)
                for tokens in tokens_seen:
                    self.assertEqual(micro_cny(official, tokens), micro_cny(today, tokens))

    def test_route_costs_the_server_already_has_are_kept(self):
        settings = {'credit_face_value_cny': FACE, 'usd_cny_rate': 7.25,
                    'route_costs': {'kimera-direct/gpt-5.6-sol': {'cost_multiplier': 0.05}}}
        defaults = script.settings_defaults(script.POLICY, settings)
        self.assertEqual(defaults['route_costs'],
                         {'kimera-direct/gpt-5.6-sol': {'cost_multiplier': 0.05}, **HANYUE_ROUTE})

    def test_a_malformed_route_is_refused(self):
        policy = copy.deepcopy(script.POLICY)
        policy['upstream_price_basis'] = {'claude-opus-5-5': [2, 25, 6.25, 0.5]}
        with self.assertRaises(PreconditionFailed):
            script.route_costs(policy)


class MigrateTests(unittest.TestCase):
    before = config([mapping('claude-opus-5')], [live_version('claude-opus-5')])

    def test_publishes_revision_checked_reads_back_and_waits_for_activation(self):
        update, _ = script.publication(self.before, script.POLICY, 1000, 1030)
        # The server stamps when the settings and each new official price changed.
        stamped = {name: {**price, 'updated_at_secs': 1001}
                   for name, price in update['settings']['official_prices'].items()}
        after = {**copy.deepcopy(self.before), 'revision': 'new',
                 'settings': {**update['settings'], 'rate_updated_at_secs': 1001,
                              'official_prices': stamped},
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
                                   'revision': 'new', 'models': ['claude-opus-5'],
                                   'official_prices_added': ['claude-opus-5'],
                                   'no_official_price': [],
                                   'costs_unchanged_on': ['kimera-primary/claude-opus-5']})

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

    def test_an_older_server_is_refused_before_anything_is_sent(self):
        # What a server before the release sends: no plan catalog.
        older = {key: value for key, value in self.before.items()
                 if key not in ('plans', 'cards_by_plan')}
        call, record = MagicMock(return_value=({'config': older}, 1000)), MagicMock()
        for dry_run in (False, True):
            with self.subTest(dry_run=dry_run):
                with self.assertRaisesRegex(PreconditionFailed, 'older than the release'):
                    script.migrate(call, record, dry_run=dry_run)
        self.assertEqual(call.call_count, 2)
        call.assert_called_with('commercial-config')
        record.assert_not_called()

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


class OfficialPriceTableTests(unittest.TestCase):
    """The migration fills the official price table, and proves route by route that no request
    costs more or less for it."""

    @staticmethod
    def world():
        """Every model of the policy, sold through the provider it is sold through, at the
        price published from the policy."""
        models = [mapping(model, PROVIDERS.get(model, 'kimera-primary'))
                  for model in script.POLICY['official_prices_per_m']]
        versions = [live_version(model, PROVIDERS.get(model, 'kimera-primary'))
                    for model in script.POLICY['official_prices_per_m']]
        return config(models, versions)

    def test_every_route_costs_what_it_did_and_now_follows_its_providers_multiplier(self):
        before = self.world()
        update, problems, proof = script.plan(before, script.POLICY, 200, 230)
        self.assertEqual(problems, [])
        models = sorted(script.POLICY['official_prices_per_m'])
        table = update['settings']['official_prices']
        self.assertEqual(table, {model: table_entry(model) for model in models})
        # Claude Opus 5.5 is priced as Claude Opus 5, and costed at it off its measured route.
        self.assertEqual(table['claude-opus-5-5']['input_usd_per_m'], 5.0)
        self.assertEqual(proof['official_prices_added'], models)
        self.assertEqual(proof['no_official_price'], [])
        routes = sorted(f"{PROVIDERS.get(model, 'kimera-primary')}/{model}" for model in models)
        self.assertEqual(proof['costs_unchanged_on'], routes)
        after_versions = before['versions'] + update['versions']
        for model in models:
            provider = PROVIDERS.get(model, 'kimera-primary')
            with self.subTest(route=f'{provider}/{model}'):
                route = (mapping(model, provider), provider, model)
                was = script.route_cost(before['settings'], before['versions'], 'default',
                                        *route, 200)
                will = script.route_cost(update['settings'], after_versions, 'default', *route, 230)
                # Costed from official prices now, at exactly the CNY per million it was.
                self.assertEqual(will[0], 'official')
                self.assertEqual(will[1], was[1])
                for tokens in TOKENS_SEEN:
                    self.assertEqual(micro_cny(will[1], tokens), micro_cny(was[1], tokens))
        # A provider's 成本倍率 now reaches a route that had only its version's cost.
        raised = {**update['settings'], 'provider_cost_multipliers': {
            **update['settings']['provider_cost_multipliers'], 'kimera-primary': 0.1}}
        self.assertEqual(
            script.route_cost(raised, after_versions, 'default', mapping('claude-opus-4-8'),
                              'kimera-primary', 'claude-opus-4-8', 230),
            ('official', [5 * 0.1 * 1.0, 25 * 0.1 * 1.0, 6.25 * 0.1 * 1.0, 0.5 * 0.1 * 1.0]))
        # Run again, there is nothing left to do, and it still shows every route unchanged.
        done = config(before['models'], after_versions, update['settings'])
        again = script.plan(done, script.POLICY, 300, 330)
        self.assertEqual(again[:2], (None, []))
        self.assertEqual(again[2]['costs_unchanged_on'], routes)

    def test_a_route_whose_cost_would_move_is_named_and_nothing_is_published(self):
        cases = [
            # 15 x 0.06 in binary is not the 0.9 the version was published with.
            ([mapping('claude-sonnet-4-6', 'kimera-direct')],
             [live_version('claude-sonnet-4-6', 'kimera-direct')],
             'route kimera-direct/claude-sonnet-4-6 (claude-sonnet-4-6 in group-pro-plus): costs '
             '[0.18, 0.9, 0.225, 0.018] today, [0.18, 0.8999999999999999, 0.22499999999999998, '
             '0.018] after the official price table is filled'),
            # A fallback on another provider is costed at its target's version today.
            ([mapping('claude-opus-5', fallback_chain=[
                {'provider_id': 'kimera-direct', 'target_model': 'claude-opus-5'}])],
             [live_version('claude-opus-5')],
             'route kimera-direct/claude-opus-5 (claude-opus-5 in group-pro-plus): costs '
             '[0.4, 2.0, 0.5, 0.04] today, [0.3, 1.5, 0.375, 0.03] after the official price '
             'table is filled'),
            # A fallback to a model with no price is costed at an estimate today.
            ([mapping('claude-opus-5', fallback_chain=[
                {'provider_id': 'kimera-primary', 'target_model': 'claude-opus-4-8'}])],
             [live_version('claude-opus-5')],
             'route kimera-primary/claude-opus-4-8 (claude-opus-5 in group-pro-plus): costs an '
             'estimate today, [0.4, 2.0, 0.5, 0.04] after the official price table is filled'),
        ]
        for models, versions, problem in cases:
            with self.subTest(route=problem.split(' (')[0]):
                before = config(models, versions)
                _, problems = script.publication(before, script.POLICY, 200, 230)
                self.assertEqual(problems, [problem])
                call = MagicMock(return_value=({'config': before}, 1000))
                with self.assertRaisesRegex(PreconditionFailed, problem.split(' (')[0]):
                    script.migrate(call, MagicMock())
                call.assert_called_once_with('commercial-config')

    def test_an_official_price_the_server_has_is_kept_and_a_model_it_lacks_is_said(self):
        own = {'input_usd_per_m': 5.0, 'output_usd_per_m': 25.0, 'cache_creation_usd_per_m': 6.25,
               'cache_read_usd_per_m': 0.5, 'note': '厂商官网 9/20', 'updated_at_secs': 7}
        before = config([mapping('claude-opus-5'), mapping('claude-sonnet-5'),
                         mapping('glm-5', retired=True)],
                        [live_version('claude-opus-5'), live_version('claude-sonnet-5')],
                        settings={'credit_face_value_cny': FACE, 'usd_cny_rate': 7.25,
                                  'official_prices': {'claude-opus-5': own}})
        update, problems, proof = script.plan(before, script.POLICY, 200, 230)
        self.assertEqual(problems, [])
        self.assertEqual(update['settings']['official_prices'],
                         {'claude-opus-5': own, 'claude-sonnet-5': table_entry('claude-sonnet-5')})
        self.assertEqual(proof['official_prices_added'], ['claude-sonnet-5'])
        # An upstream model the policy has no price for is left out and said.
        lacking = config([mapping('claude-opus-5', fallback_chain=[
            {'provider_id': 'kimera-primary', 'target_model': 'glm-5'}])],
            [live_version('claude-opus-5'), {**live_version('claude-opus-4-8'), 'model': 'glm-5'}])
        _, problems, proof = script.plan(lacking, script.POLICY, 200, 230)
        self.assertEqual((problems, proof['no_official_price']), ([], ['glm-5']))
        self.assertEqual(proof['costs_unchanged_on'],
                         ['kimera-primary/claude-opus-5', 'kimera-primary/glm-5'])

    def test_a_dry_run_says_what_it_adds_and_which_routes_it_shows_unchanged(self):
        before = config([mapping('claude-opus-5')], [live_version('claude-opus-5')])
        call = MagicMock(return_value=({'config': before}, 1000))
        summary = script.migrate(call, MagicMock(), dry_run=True)
        self.assertEqual((summary['official_prices_added'], summary['costs_unchanged_on']),
                         (['claude-opus-5'], ['kimera-primary/claude-opus-5']))


class HistoricalFaceValueTests(unittest.TestCase):
    def test_migration_freezes_the_original_face_value_once(self):
        settings = {'credit_face_value_cny': 0.03}
        first = script.settings_defaults(script.POLICY, settings)
        self.assertEqual(first['legacy_credit_face_value_cny'], 0.03)
        later = script.settings_defaults(script.POLICY, {**settings, **first, 'credit_face_value_cny': 0.10})
        self.assertEqual(later['legacy_credit_face_value_cny'], 0.03)


if __name__ == '__main__':
    unittest.main()
