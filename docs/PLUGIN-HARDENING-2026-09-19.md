# Plugin interoperability strengthening

Reference: local kiroapi-8.0.1.vsix, verified extracted package and bundle match archive.

## Implemented

- Parse reasoning controls at the four request positions used by the reference plugin; context, current input, root, then conversation. Only typed low/medium/high/xhigh/max values are accepted. Unknown fields are not forwarded.
- Preserve optional standalone Kiro systemPrompt; existing serialized-input validation and reservation estimates include it, and administrator group prompts retain their existing prefix behavior.
- Carry effort through translation. OpenAI uses low/medium/high (xhigh/max degrade to high). Claude Sonnet/Opus 4.6 use adaptive thinking (max only for Opus); other Anthropic targets use budgets 1024/4096/8192/16384/24576 capped below reserved output. A ceiling can make several budget tiers equivalent. No output-limit increase is performed.
- Remove sampling temperature when reasoning is requested. Reject reasoning for models without the configured capability, releasing reservations.
- No inference controls means unchanged provider request behavior.

## Limits

Provider support must be configured truthfully. This does not guarantee every vendor model accepts these controls. Provider fallback targets also need compatible capabilities.

No official-account key rotation, automatic authorization of discovered upstream models, or default prompt-cache injection was added. Existing cache token accounting is not an active cache policy. Multi-window recovery and full IDE visible interaction remain separate acceptance tasks.

## Verification

Run gateway reasoning_controls_test and serial gateway tests. Production deployment status must be checked separately; this document is not a deployment or GUI acceptance certificate.

### Local and upstream results

- Final serial gateway suite: 168 passed, zero failed. Log: `.acceptance/plugin-strengthening-tests.log`.
- kiro-wire suite: 23 passed.
- Strict gateway library Clippy: passed.
- Isolated direct upstream probe with adaptive thinking/low effort: HTTP 200, end_turn, visible OK reply. This proves parameter acceptance, not equivalent semantics across all models.
- Earlier in-progress build results are superseded by the final serial run above.
