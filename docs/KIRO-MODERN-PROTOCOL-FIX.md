# Kiro 1.1.14 Protocol Compatibility Fix

## Observed Failure

The real IDE session `sess_9e180905-031e-4538-9756-fa266e0907ab` logged model refresh HTTP 404 and runtime `ResourceNotFoundException: Cannot POST /`. Account usage was independently available. This is distinct from the previous TLS hostname failure.

Read-only inspection of the installed Kiro agent SDK found:

- Control-plane `ListAvailableModels`: `GET /List-Available-Models`.
- Its `defaultModel` response member is a Model object, not the legacy string.
- Runtime AWS JSON requests use `POST /` and `x-amz-target: KiroRuntimeService.<operation>`.
- GenerateAssistantResponse uses `conversationState`; InvokeMCP uses JSON-RPC members; feature configuration returns a `configuration` document.

## Change

`crates/gateway/src/facade/modern.rs` registers the modern model alias and a strict target allowlist. Adapters reference the final configured legacy handlers instead of constructing fresh providers or billing engines. Modern model responses convert only `defaultModel`; legacy responses are unchanged. Runtime generation remains an unbuffered handler response, preserving EventStream streaming and existing accounting. Feature configuration serves an empty configuration (defaults), not fabricated enabled capabilities.

Both authenticated and test routers register compatibility endpoints. Production auth middleware applies to the new endpoints, including feature configuration. Unknown services/operations cannot select arbitrary handlers. No certificate validation bypass, client installation modification, or customer card activation is part of this fix.

## Local Verification

- `cargo test -p gateway --tests --quiet -- --test-threads=1`: 163 passed.
- Desktop bridge/UI regression: 13 passed (mocked workflow, not live IDE acceptance).
- `cargo clippy -p gateway --lib -- -D warnings`: passed.
- Five modern protocol tests cover legacy/new schema separation, anonymous access, operation allowlisting, configured handler reuse, tool discovery, EventStream framing and immediate card revocation.
- Parallel suite runs showed intermittent existing stream-resilience failures (Windows persistence access denied / missing settlement). The serial run passed; this does not establish that the parallel flakiness is fixed.

## Deployment Verification

Run `test_modern_deployed.py` with SSH credentials via stdin, never command-line arguments or checked-in files. It issues one isolated card, checks modern model routes and runtime operations, makes one small real upstream call, verifies billing, and bans the test card in cleanup. Results: `.acceptance/modern-deployed-results.json`.

Live IDE model selection and visible chat completion are a separate acceptance boundary. At deployment preparation, the local client was signed out with no takeover snapshot and Kiro stopped. Do not count the protocol probe as GUI acceptance or reactivate customer credentials automatically.

### Production Result (2026-09-19 Asia/Shanghai)

- Deployed `kiro-byok:20260918T165338Z`; container reports healthy. Previous release and data backup retained.
- Local direct HTTPS with the pinned CA passed all 10 checks in `test_modern_deployed.py`, including real upstream streamed OK, credit debit, duplicate invocation protection, and immediate test-card revocation. No customer card was used.
- The stream assertion concatenates content fragments before checking the reply and requires end_turn.
- Local proxy transport intermittently failed SSH negotiation and HTTPS with TLS EOF; final deployment/probe used direct connections without disabling certificate verification.
- Full visible IDE interaction remains unverified; protocol success is not GUI acceptance.
