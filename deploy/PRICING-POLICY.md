# Kimera retail policy

Current policy: `kimera-gpt-cost015-retail025-claude035-20261003`.

| Model family | Kimera cost multiplier | Retail multiplier |
| --- | ---: | ---: |
| GPT (gpt-6-astra, gpt-5.6-sol; terra retired) | 0.15 | 0.25 |
| Claude | 0.08 | 0.35 |

`pricing_policy.json` keeps the default retail multiplier plus explicit `model_retail_multipliers`. Retail overrides follow the exposed model, not a mutable provider route. Cost multipliers remain provider-specific. Opus 5.5 retains its approved Opus 5 pricing alias; retired Hanyue historical cost metadata is not a live Kimera cost basis.

Credit face value remains CNY 0.03. The configured official-price numeric conversion coefficient is 1, not live USD/CNY FX. Historical face value, customer ledgers and package prices must not be rewritten. Plan discounts reduce realized cash margins versus face-value margins.

Changes to existing rates require revision-checked, effective-dated new price versions; preserve all previous versions. Do not treat a migration no-op on existing official-price blocks as proof that rates were republished. The legacy `publish_pricing.py` bootstrap is intentionally disabled for the new policy before reading credentials or accessing the network: it still encodes historical 0.24 pricing.

Previous retail-policy activation: 2026-09-27 22:01:56 UTC / 2026-09-28 06:01:56 +08:00.
Previous retail-policy revision: `ffc7a99bbbf9a26cf5ef94c7e710deda05d42fe9b6da93fb75d920c42fe69f4b`.
All 10 models were reconciled against exact integer-microcredit ledger debits and rounded micro-CNY upstream cost. Per-request retail charges round upward; upstream costs round to nearest micro-CNY, half away from zero. Profit reports are gross contribution before infrastructure, payment fees, refunds and tax, not net profit.

Never store upstream keys in policy files, receipts, reports or source control. Keep release secrets on the existing protected input/config path.

## Upstream maintenance 2026-10-03

GPT remains on `kimera-direct` at its existing HTTPS address. Its stored key was rotated,
with only `gpt-5.6-sol` and `gpt-6-astra` allowed. Terra remains retired, not merely hidden.
GPT cost is official price x 0.15; retail stays x 0.25. New effective-dated versions preserve
all prior price versions, customer prices and the CNY 0.03 credit face value.

Provider `88888ai-claude` (`https://88888ai.cc`, Anthropic format) and its key are disabled.
No primary or fallback model route points to it. Its staged cost multiplier is 1.0, an
official-price baseline, not a verified discounted supplier cost. Existing Claude routes,
costs and retail prices are unchanged.

The live official-price reference catalog includes 42 discovered names: 31 Claude names
(including explicitly noted date/spelling aliases) and 11 GPT text names. Sources:
https://developers.openai.com/api/docs/pricing and
https://platform.claude.com/docs/en/about-claude/pricing, plus the official Sonnet 3.5/3.7
launch articles. These reference entries do not expose additional models or certify routing.
Upstream `cc-*` aliases are unpriced pending an explicit mapping. GPT proprietary aliases
and image models remain unpriced/unpublished; image pricing cannot be represented faithfully
by this text-token four-field rate card. Long-context and one-hour-cache pricing are not
represented by these standard short-context / five-minute-cache reference prices.

Activation: 2026-10-02 21:34:08 UTC (2026-10-03 05:34:08 Asia/Shanghai).
Revision: `7639750afb2f0cb7f76c2f65a936d27471bd2a95480626cf931ce6098a43cb35`.
Read-back verified historical versions and existing Claude model routes unchanged. Both
stored keys passed model discovery. Sol and Astra each passed one minimal server-side
upstream probe (HTTP 200). No Claude generation request was sent and no real IDE end-to-end
acceptance was performed. Non-secret detailed receipts are local acceptance evidence.
