# Reference refresh and focused hardening (2026-09-19)

## Refresh

Pinned revisions are recorded in `.acceptance/reference-revisions.json`.
Ten clean reference repositories were fetched and fast-forwarded where needed;
their HEADs match their fetched tracking branches. This means latest fetched
branch state, not a guarantee that every project has a newer release tag.

`kiro-account-manager` has 13 modified files and was 71 commits behind. Its
working tree remains untouched. `references/kiro-account-manager-upstream` is a
detached worktree at fetched upstream commit `799bb0e`, used for comparison.
The VSIX reference was not changed.

## Findings and adoption decisions

| Reference | Examined capability | Decision |
| --- | --- | --- |
| Kiro-Go | Empty/truncated streams and retries after client output | Added rejection of terminal-only empty responses. Existing unexpected-EOF failure and partial-output settlement remain in place. No automatic replay after output. |
| AnyBridge | Per-protocol reasoning effort conversion | Existing reasoning/systemPrompt implementation retained. Do not infer authorization or pricing from model-name suffixes. |
| cursor-byok | WebSearch query aliases and tool codecs | Cursor-specific protobuf codecs are not applicable to Kiro MCP. Current MCP declares `query`; do not advertise search without a configured backend. |
| kiro-custom-model-hijacker | Dynamic endpoint and non-chat route filtering | Retain existing explicit Kiro RPC routing; do not copy broad hostname substring interception. |
| antigravity-studio | Changelog: orphan tool pairing, local proxy hardening | Repository contains release documentation, not implementation source. Existing chronological tool-pair repair already covers orphan results; no duplicate implementation added. |
| kiro.rs | Anthropic compatibility and new model release | No automatic publication of upstream model names without group/pricing configuration. |
| ZyphrZero-kiro.rs | Custom model IDs and reasoning capability declarations | Existing explicit model mappings/capabilities retained; avoid implicit suffix-based access expansion. |
| kiro-gateway | Transparent protocol adaptation | Preserve user content and existing explicit protocol adapters. |
| kiro-account-manager | Latest tree isolated from local edits | Latest comparison worktree created; no destructive merge or reset. |
| chaogei-kiro-account-manager | Account switching and refresh overview | Account-pool rotation is outside the card-authorized BYOK gateway flow. Not transplanted. |
| AegisCompiler | Project structure and licensing-service scope | Compiler/protection framework not needed for this stream fix. Not transplanted. |

## Code change

`crates/gateway/src/stream.rs` now rejects `Done` with no output. Input-only
usage from that empty response does not debit credits; reservation is released,
no successful idempotency entry is stored, and the invocation can be retried.
Empty tool fragments are not counted as output. OpenAI and Anthropic adapters
now reject in-band SSE JSON errors rather than ignoring them; public errors
do not include upstream diagnostic content.

Existing nonempty interrupted responses retain partial-settlement semantics;
this is deliberately not a blanket refund for all interrupted generations.

Regression covers empty text, a stop reason plus Done, both with and without
reported input usage, reservation release, no debit, no successful metadata,
and retry eligibility. Normal tool/text streams remain covered by existing tests.

## Verification scope

Targeted stream tests: 15 passed. Full gateway tests, wire tests and strict
Clippy logs are under `.acceptance/reference-*.log`.
This is a focused reference review, not a claim that all upstream source files,
all optional backends, or real IDE GUI interactions have been audited.

Additional provider regression covers error objects and `type:error` events in
both supported protocols. Patch-engine tests initially hit HTTP 502 for a
loopback mock through the local proxy; all passed with process-local
`NO_PROXY=127.0.0.1,localhost,::1`. No system proxy settings were changed.

Final local verification: reference-hardening-tests.log: 170 passed, reference-wire-tests.log: 23 passed, reference-patch-tests.log: 52 passed, reference-billing-tests.log: 117 passed. Strict gateway Clippy and workspace formatting passed.

## Production

Released `kiro-byok:20260918T185653Z`; gateway healthy. Final source archive
SHA-256: `752b9bbbcdce268228a36a5817d847d242bf4648258a8851420a9087e8f8274e`.
All 11 modern production protocol checks passed, including a real upstream
response, credit debit, duplicate-invocation protection, invalid reasoning
rejection without debit, and disposable-card revocation. Both direct and
local-proxy TLS-verified health checks returned HTTP 200 after promotion.
Evidence: `.acceptance/reference-deployed-verification.json`.

Empty/error upstream scenarios were exercised locally, not injected into
production. This release did not perform real IDE GUI acceptance.
