# v0.6 implementation plan and release gates

Source baseline: validated v0.5 archive SHA-256 dc6962042b149f869d30de23e7578af58765726cc1738a95d42156c31ec5306d. This archive is not Git ancestry. Windows development baseline is b84e63d; no push or publication is authorized.

1. Preserve and hash the complete source baseline; run existing checks on the cloud toolchain when available.
2. Implement shared classification policy, stable content identity, bounded result cache and independent manual overrides; add exhaustive/conflict/identity tests.
3. Implement bounded, cancellation-aware local metadata and optional model executor contracts. Preserve source bytes. Missing/unapproved model means awaiting model, never real/non-AI. C2PA legacy validation remains separate; without a trusted snapshot no source claim becomes trusted.
4. Integrate compact folder filters, counts, non-destructive badges, details, manual correction, independent scan/badge controls and privacy controls. Current selection stays anchored when its filter membership changes.
5. Integrate only a verified model/runtime package. Experimental models remain manual-only until held-out quality, resource, licensing and platform gates pass. No dummy detector or heuristic pixel classifier.
6. Run formatting, tests, clippy, profile builds, UI render/regression tests and adversarial cancellation/cache tests. Preserve existing rotation, clipboard, animation and export tests.
7. Package source-only handoff with per-file preimages, safe patch verification and Windows QA instructions. Preserve EXE-only end-user distribution and no Git ancestry claims.

## Implementation addendum

- AI preferences and SQLite records use a separate versioned store; v0.5 settings.json schema remains unchanged for safe upgrade.
- Global source checking may read bounded generation metadata; it does not claim general AI recognition. A metadata positive is only suspected AI.
- Automatic pixel classification cannot be enabled by arbitrary imported JSON claiming a validated threshold. A shipped allowlist of approved package identities is required. Until the independent release gate passes, pixel model output is experimental and never silently enters automatic labels.
- All machine/manual state is keyed by full content SHA-256 plus filesystem identity. Size/mtime alone are never considered proof of unchanged content. Revalidation is required when reopening/refreshing.
- Resource budgets and cancellation latency are targets unless backed by measured reports. Linux cloud tests do not substitute for Windows native, setup upgrade, accessibility or DPI acceptance.
