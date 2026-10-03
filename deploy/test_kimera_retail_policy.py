"""Current per-model policy; independent of historical migration fixtures."""
import unittest
from deploy import migrate_official_pricing as script

class CurrentRetailPolicyTests(unittest.TestCase):
    def test_all_models_use_authorized_family_rates(self):
        for model in script.POLICY['official_prices_per_m']:
            gpt = model.startswith('gpt-')
            mapping = {'exposed_model_id':model,'target_model':model,
                       'target_provider_id':'kimera-direct' if gpt else 'kimera-primary'}
            block = script.official_block(mapping, script.POLICY, 0.03)
            self.assertEqual(block['price_multiplier'], 0.25 if gpt else 0.7)
            self.assertEqual(block['cost_multiplier'], 0.15 if gpt else 0.13)
            self.assertNotIn('cost_basis_usd_per_m', block)
            if model == 'claude-opus-5-5':
                self.assertEqual([block[k + '_usd_per_m'] for k in script.PRICES], [4, 20, 5, 0.2])
                self.assertEqual(script.costs(block), [0.52, 2.6, 0.65, 0.026000000000000002])

    def test_gpt_override_does_not_follow_route_provider(self):
        mapping = {'exposed_model_id':'gpt-5.6-sol','target_model':'gpt-5.6-sol','target_provider_id':'unknown'}
        self.assertEqual(script.official_block(mapping, script.POLICY, 0.03)['price_multiplier'], 0.25)

    def test_staged_claude_provider_uses_official_cost_baseline(self):
        mapping = {'exposed_model_id': 'claude-opus-5-5',
                   'target_model': 'claude-opus-5-5',
                   'target_provider_id': '88888ai-claude'}
        block = script.official_block(mapping, script.POLICY, 0.03)
        self.assertEqual(block['cost_multiplier'], 1.0)
        self.assertEqual(block['price_multiplier'], 0.7)
        self.assertEqual(script.costs(block), [4, 20, 5, 0.2])
