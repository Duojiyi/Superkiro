# Template intent and delivery release review

## Behavior

- Optional `intent` mode: AND concept groups, OR synonyms, normalized casing/full-width punctuation, exact configured model. Pelican defaults bind the subject and riding action in one phrase group, including reversed Chinese phrasing.
- Exact and contains legacy modes retain their explicit matching contracts. Convert the live pelican rules to intent mode and disable the redundant legacy alias during publication.
- Ambiguous/negated/quoted/editing requests, unsupported subjects, and explicitly prohibited file writes fall through to upstream. This is a conservative configured matcher, not unrestricted semantic understanding or a zero-error guarantee.
- Admin draft preview calls the same server matcher without generating content, charging, or calling upstream. HTML remains inert editable text.
- Optional delivery timeline: editable messages at absolute offsets, random bounded file dispatch, editable dispatch/success/error/unknown/replay/continuation wording. Default text identifies the preset template and does not fabricate model thinking or upstream tokens.
- Each selected model retains its own HTML, timeline and exact microcredit price. Production publication requests 9,500,000 microcredits and 170–230 seconds, with text at 10 and 40 seconds.
- Protocol keepalives are independent of visible text and use runtime `keepalive_secs`. Lazy streaming owns wait/idempotency/capacity guards; cancellation before file dispatch does not debit. Config/card/balance are checked again immediately before durable debit. Receipts preserve the delivery snapshot and provide free replay/acknowledgement.

## Independent audits

Two independent reviewers inspected matching, protocol behavior, accounting, compatibility and admin validation. Findings fixed before release:

1. Explicit no-file directives could match: add conservative file-operation refusal checks and bilingual negative regressions.
2. Subject/action coincidence could match a different scene: bind riding to pelican phrases; share the exact default fixture between Rust and frontend regressions.
3. Empty/sparse timeline could timeout: add protocol keepalives with interval/cancellation/retry/refusal regressions.
4. Intent restrictions changed legacy contains semantics: apply them to intent mode only.
5. New nullable fields expanded near-limit legacy snapshots: omit absent fields in Rust/JS serialization, test the 2MiB-minus-one boundary and receipt round trips.
6. Save-recovery equality: ignore JSON object key order and optional null/absent differences, while preserving prices and array order.

## Validation and limitations

- Full Rust workspace tests excluding desktop-host; targeted billing/matcher and gateway template regressions; strict Clippy.
- Admin unit/contract tests, real browser fixture and production build; desktop 191 tests and local dev executable build.
- Desktop browser checks at 480x540 and 620x820 confirm visible remaining-balance bar, working usage controls and no horizontal overflow/JavaScript errors.
- No guarantee that an arbitrary paraphrase or typo matches. Uncertain requests intentionally go upstream. Administrators should preview positive and negative examples before changing phrases.
- A network failure after durable charging but before file receipt requires replay using the same invocation ID; no claim of automatic delivery across arbitrary client behavior. No real process-crash fault injection performed.
- Production release status, configuration CAS result and streamed live accounting acceptance are recorded separately under `.acceptance`; this document alone is not evidence of deployment.
