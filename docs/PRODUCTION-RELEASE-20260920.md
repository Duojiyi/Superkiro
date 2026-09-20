# Production Release · 2026-09-20

## Deployed State

- Public site: https://kiro.rent/ ; docs: /docs ; device management: /device ; admin: /admin/.
- Backend and native artifact source: `396edefab69ad0829469e0568139e9e11b984b81`.
- CI passed: https://github.com/Duojiyi/Superkiro/actions/runs/35497709392
- Windows/macOS native workflow passed: https://github.com/Duojiyi/Superkiro/actions/runs/35497709384
- Active release: `/opt/kiro-byok/releases/20260920T074237Z`.
- Image: `kiro-byok:20260920T074237Z`, ID `sha256:dfece8b9f1f9028398ff5abb7e7c2b07fa54ff0531e8d63af6ce788c2803bcc9`.
- Portal received a static-only follow-up in this commit: repair the misplaced keyframes closing brace and expose third-party notices. No backend/client rebuild is represented by this follow-up.
- Final public portal SHA256: `8d182c3d8fd56167c8c57499c7b429aec9ee410ef02906a52d9f338d9c9eeaa5`.

## Verification

- 41 live API/end-to-end checks passed, including six real model responses, debit replay without duplicate charge, device replacement, and old access/refresh-token invalidation.
- 19 production browser checks passed again after the final static patch: desktop/mobile pages, read-only card query, confirmed unbind, actual admin login, nine navigation pages, secure cookies, CSRF and logout revocation.
- Desktop/mobile Windows and both Mac download links match the live manifest. No page JavaScript errors or horizontal overflow in the checked views. A CSS computed-style regression assertion now detects the malformed keyframes rule.
- All three public binaries were fully downloaded and verified against exact approved SHA256 and size. Preview approval is based on CI and package inspection, not a claim of full native IDE acceptance.
- Disposable acceptance cards were banned in cleanup. Their issuance changes aggregate card statistics; the pre-promotion statistics comparison was performed before these tests.

## Published Downloads

Version: `0.1.0-preview.20260920.396edef`. All packages are **unsigned preview builds**.

- windows x64: [download](https://kiro.rent/downloads/Superkiro-0.1.0-preview.20260920.396edef-a4a90e92ca407fe091b4961e4cb27e3eef2739b273affe463cb6edb3d6201073-windows-x64.exe); 13068288 bytes; SHA256 `a4a90e92ca407fe091b4961e4cb27e3eef2739b273affe463cb6edb3d6201073`.
- macos arm64: [download](https://kiro.rent/downloads/Superkiro-0.1.0-preview.20260920.396edef-de1559b3fb7b89d0e7a9ca7f7325f67ede54566737fd4e8934d693e21cdbf8b0-macos-arm64.app.tar.gz); 4831021 bytes; SHA256 `de1559b3fb7b89d0e7a9ca7f7325f67ede54566737fd4e8934d693e21cdbf8b0`.
- macos x64: [download](https://kiro.rent/downloads/Superkiro-0.1.0-preview.20260920.396edef-eb12b2d94c98b2ce21760c24282cc026d26c492edb1e4f6b409a88f2ade324a9-macos-x64.app.tar.gz); 5115125 bytes; SHA256 `eb12b2d94c98b2ce21760c24282cc026d26c492edb1e4f6b409a88f2ade324a9`.

Third-party dependency notices (including build/all-target dependencies): https://kiro.rent/downloads/THIRD-PARTY-NOTICES-20260920-c8819645a40a.txt . Publicly linked from the download section; SHA256 `c8819645a40a76fe9bc5ca00be80ebeb9509327ba315f56a48b14013ceb4e94f`.

## Recovery and Boundaries

- Previous release retained: `/opt/kiro-byok/releases/20260919T205821Z`.
- Verified stopped-data/config backup: `/opt/kiro-byok/backups/release-20260920T074237Z`. Protected credentials, KEK and pre-promotion account aggregates were preserved.
- Pre-follow-up portal retained as `index.html.before-notices` in the current portal directory. Restoring it also restores the older CSS defect.
- Post-deploy `kiro-backup.service`: success / exit 0; `kiro-backup.timer`: active. Offsite backups are not configured or verified.
- Gateway healthy; Caddy running; no deployment lock left. Disk available approximately 21 GB.
- Windows is not Authenticode-signed; Mac is not Developer ID-signed or notarized. Full real-device Mac/IDE acceptance remains outstanding and is disclosed on the site.
- Audit results cover tested paths, not a guarantee of zero defects. Batch-issue/announcement uncertainty guards are same-tab session storage, not server-wide idempotency.
- Existing GTK/glib advisory warning remains relevant to future Linux packaging; no Linux package is released here.
- Rotate the root password shared in chat and prefer SSH keys; credentials are excluded from this report.

This production record supersedes the historical local-only release state in `apps/admin-ui/ADMIN-ITERATION-READINESS.md`.
