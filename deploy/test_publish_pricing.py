import unittest
from publish_pricing import version,POLICY
class PricingTest(unittest.TestCase):
 def test_all_rates_and_unit_economics(self):
  from decimal import Decimal
  for model,prices in POLICY['official_prices_per_m'].items():
   v=version(model,'default',100)
   for kind,price in zip(('input','output','cache_creation','cache_read'),prices):
    self.assertEqual(Decimal(str(v[kind+'_price_per_m'])),Decimal(str(price))*Decimal('0.08'))
    self.assertEqual(v['fixed_'+kind+'_credit_per_m'],int(Decimal(str(price))*8*1000000))
  for tier in POLICY['tiers']:
   cost=tier['points']/8*.08
   self.assertGreaterEqual((tier['price_cny']-cost)/tier['price_cny'],.6)
 def test_one_credit_opus_example(self):
  v=version('claude-opus-5','default',100)
  micro=sum(v['fixed_'+k+'_credit_per_m']*n/1000000 for k,n in [('input',10000),('output',2000),('cache_read',50000)])
  self.assertEqual(micro,1000000)
if __name__=='__main__':unittest.main()
