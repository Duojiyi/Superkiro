# Kiro Credit Display Coverage

## Verified mapping (Kiro 1.1.14)

`GET /getUsageLimits` returns full-precision credit values. The extension's
`kiro.usageLimits.getUsageLimits` command maps `currentUsageWithPrecision` and
`usageLimitWithPrecision` into `data.usageBreakdowns[].currentUsage` and
`usageLimit`, falling back to the non-precision fields. These values also feed
percentages and usage notifications; rounding this shared model is not a
presentation-only fix.

The extension's `getItemContent` status-bar formatter interpolates these values
into strings. `crates/patch-engine/src/patch.rs` replaces only its known display
expression, including bonus and overage credit counts. It uses Intl halfExpand
(half up for nonnegative credits) with one fractional digit. Monetary charges
remain at their original precision. This change is included in initial patching
and reapplication, with the existing authenticated backup/restore lifecycle.
The desktop UI's shared `toLocaleString` with `maximumFractionDigits: 1` is also
acceptable: at most one decimal, without requiring trailing `.0`.

## Compatibility fallback

The status-bar repair is optional and targets the observed expression, not the
minified function name. There is no exactly-one-match requirement. No matching
expression means no display change, while normal endpoint injection continues.
Repeated exact expressions are all repaired; already repaired expressions remain
unchanged. The ordinary endpoint validation and JavaScript syntax gate still
apply. This is not a promise of rounded display on unrecognized Kiro versions.
The one-match check used during read-only verification of the installed version
was diagnostic only, not a production compatibility gate.

## Unchanged workbench account label

Read-only inspection of Kiro 1.1.14 found the account rail requesting
`kiro.usageLimits.getUsageLimits` and rendering:

```javascript
eo.textContent = `${lo} ${In.currentUsage}/${In.usageLimit}`;
this.applyCollapsedAccountLabel(/* account name */, eo.textContent);
```

Both the expanded label and its collapsed label reuse this unrounded string.
The code lives in `resources/app/out/vs/workbench/workbench.desktop.main.js`,
which is explicitly covered by `resources/app/product.json`'s `checksums`.
It is not part of the extension patch target. The existing takeover snapshot
stores a single `extension_path`; patch hashes, crash recovery, doctor checks,
and official restore track that extension only. No workbench backup was present
in the inspected installation.

Therefore the workbench label remains unrounded. No API rounding, shared command
result rounding, number-prototype override, checksum modification, or untracked
workbench write was introduced. Complete coverage of all Kiro versions/surfaces
has not been established.

## Alternatives

1. Prefer an upstream Kiro renderer fix: call `toLocaleString` with
   `maximumFractionDigits: 1` on the two values at the text-rendering site. This
   also fixes the collapsed label without changing the command response.
2. A separately scoped, opt-in workbench patch could modify that exact rendering
   site, but needs version/anchor validation, durable multi-file snapshots,
   authenticated backups, interrupted apply/restore recovery, upgrade detection,
   doctor integration, and an explicit policy for checksum/integrity warnings.
   Extending the current single-file patch silently is not justified for a
   cosmetic decimal-format change.
3. Until then, use the patched extension status bar or desktop UI for rounded
   credits; the native account rail can still show full precision.

No server deployment is needed for the extension display fix. A rebuilt local
patcher/client and patch reapplication with Kiro closed are required. Investigation
and tests do not apply patches to the installed app or launch Kiro.
