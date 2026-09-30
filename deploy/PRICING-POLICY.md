# Kimera retail policy

Current policy: `kimera-retail-gpt025-claude035-20260928`.

| Model family | Kimera cost multiplier | Retail multiplier |
| --- | ---: | ---: |
| GPT (gpt-6-astra, gpt-5.6-sol, gpt-5.6-terra) | 0.06 | 0.25 |
| Claude | 0.08 | 0.35 |

`pricing_policy.json` keeps the default retail multiplier plus explicit `model_retail_multipliers`. Retail overrides follow the exposed model, not a mutable provider route. Cost multipliers remain provider-specific. Opus 5.5 retains its approved Opus 5 pricing alias; retired Hanyue historical cost metadata is not a live Kimera cost basis.

Credit face value remains CNY 0.03. The configured official-price numeric conversion coefficient is 1, not live USD/CNY FX. Historical face value, customer ledgers and package prices must not be rewritten. Plan discounts reduce realized cash margins versus face-value margins.

Changes to existing rates require revision-checked, effective-dated new price versions; preserve all previous versions. Do not treat a migration no-op on existing official-price blocks as proof that rates were republished. The legacy `publish_pricing.py` bootstrap is intentionally disabled for the new policy before reading credentials or accessing the network: it still encodes historical 0.24 pricing.

Live activation: 2026-09-27 22:01:56 UTC / 2026-09-28 06:01:56 +08:00.
Live revision: `ffc7a99bbbf9a26cf5ef94c7e710deda05d42fe9b6da93fb75d920c42fe69f4b`.
All 10 models were reconciled against exact integer-microcredit ledger debits and rounded micro-CNY upstream cost. Per-request retail charges round upward; upstream costs round to nearest micro-CNY, half away from zero. Profit reports are gross contribution before infrastructure, payment fees, refunds and tax, not net profit.

Never store upstream keys in policy files, receipts, reports or source control. Keep release secrets on the existing protected input/config path.
