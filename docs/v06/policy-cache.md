# v0.6 shared AI policy and local cache

## Scope and release boundary

The implementation is a shared Rust evidence policy, filtering model and SQLite cache. It does not establish that an image is real, implement a validated pixel detector, approve any model package, or write image pixels, EXIF, XMP or sidecars. Source-only checks remain explicitly pending a suitable pixel model when no independent positive evidence exists.

The policy is a pure function, `policy::decide(record, manual)`, with `POLICY_VERSION = 1`. Windows and macOS use the same semantics. Display strings distinguish unknown/pending, suspected generation, authenticated generation evidence, capture records and manual decisions.

## Decision precedence

1. A manual decision is active only when its full content SHA-256 matches the current verified image identity. Active AI maps to AI; active non-AI maps to Unmarked. Both retain automatic evidence and display conflict warnings when appropriate.
2. `TrustedGeneration` requires `authenticated = true`. That boolean means signature validation, current-content binding and the current issuer trust policy all passed. An ordinary string or readable watermark does not meet this contract.
3. Unsigned generation metadata or an approved, applicable model result above its threshold yields Suspected AI. The model also needs finite values, a nonempty package fingerprint and input scope, and `high_signal = true`. Unsupported inputs cannot become AI from a model score.
4. Without a usable positive conclusion, terminal failure or timeout maps to Error.
5. An unfinished, paused, unsupported or awaiting-model check maps to Pending. A nominally Complete source-only scan also remains Pending. Unapproved model output cannot convert this into completed general AI detection.
6. Completed checks with valid approved model output but no strong signal map to Unmarked, never to verified real or verified non-AI.

Capture evidence is retained alongside generation evidence. Compatible marker records alone never imply generation, even when a marker's payload is authenticated: an adapter must emit a separate authenticated generation claim when all generation-specific conditions hold. An unauthenticated `TrustedGeneration` claim is displayed as unverified and does not silently become a trusted or suspected result.

A stale manual decision remains available for the details panel with a reconfirmation warning. The policy cannot activate a manual decision without a current record carrying a verified content hash. `MachineRecord::invalidate_model()` removes only mismatched model output, preserves independent evidence, and moves a completed record to AwaitingModel. Threshold-only changes may instead update the threshold and rerun policy.

## Filters and counts

`Filter::{All,Ai,Unmarked,Pending,Error,Manual}` has stable snake-case serialization, an `ALL` list, localized `label()`, and `matches(&Decision, include_suspected)`.

`FilterCounts::add()` maintains the invariant:

`all = ai + unmarked + pending + error`

Manual is an overlapping shortcut and is not added to that sum. Suspected is a subset of AI. `visible_count(Ai, false)` excludes suspected automatic labels without reclassifying them or changing the primary counts; active manual AI and authenticated source AI remain included. These APIs do not reorder files or control selection. The controller/UI must keep selection anchored to stable identity and apply text search as an intersection.

## Identity and record contract

- `AssetIdentity.key` is a filesystem-stable identity when available; otherwise a canonical path. Path, size and mtime alone never prove that content is unchanged.
- `content_hash` is a full-file SHA-256, freshly checked on reopen/refresh before cache reuse. Hash text must contain exactly 64 hexadecimal characters; producer-generated lowercase text is the canonical representation.
- Automatic lookup requires both stable identity and the full content hash. A same-path replacement with a different filesystem identity or different content hash cannot reuse the prior record.
- A stable-key rename or mtime change may reuse data only after a matching full hash has been verified. Cache hits update the returned non-authoritative size/mtime to the caller's current identity.
- Old automatic content versions may remain as LRU-evictable history. They are never returned for a different current hash.
- A manual row is separately keyed by asset identity and retains its original content hash. Changing content leaves the manual row intact but inactive. Explicit user reconfirmation may replace it.

Approval does not survive serialization as authority: every `get_machine()` clears the rehydrated model's `approved` field, in memory and on disk. The controller can restore approval only by checking the current shipped allowlist and full package/input contract. No approved model exists in this implementation. A package manifest or edited cache JSON cannot authorize itself.

## Cache API

All operations return `Result<_, CacheError>` unless they are simple predicates. `Cache::open(path, persistent)` takes an explicit application-data path; it never computes a path from an image or opens an image. `Cache::memory()` is the explicit all-in-memory fallback. `open_with_budget` and `memory_with_budget` support deterministic resource tests.

- `get_machine(&AssetIdentity)` and `put_machine(&MachineRecord)` read/write automatic snapshots.
- `get_manual(asset_key)` returns active or stale manual records; policy checks applicability.
- `set_manual(asset_key, override)` and `remove_manual(asset_key)` record or revoke an explicit local decision.
- `clear_automatic()` and `clear_manual()` are separate operations.
- `stats()` returns the automatic row count/accounted bytes and independent manual count.
- `set_persistent(false)` turns automatic persistence off. Automatic results continue in memory; manual decisions remain durable when a usable application-data database was supplied. Explicit clear/delete operations still affect the disk store while this setting is off. Turning persistence back on merges the session's automatic results into disk without resurrecting deleted manual records.
- `Cache::memory()` has no persistent path and cannot be switched to disk. A caller that falls back after a database error must disclose that manual changes in this fallback session are not durable.

The automatic persistence setting is not an erase command. Previously saved automatic results remain until explicitly cleared. Manual decisions intentionally remain independent of the automatic-cache budget and persistence setting. The application must label this distinction clearly.

## Capacity, validation and privacy

The default automatic budget is 100 MiB with LRU eviction. Accounting includes serialized payload, identity key, hash and 128 bytes of row overhead. Manual records are never LRU-evicted. The budget is an automatic-record accounting limit, not a promise that the SQLite file occupies exactly 100 MiB: indexes, page allocation and separately retained manual records also use disk. Newly created disk databases use full auto-vacuum to reclaim unused pages. Older migrated databases can retain SQLite free pages.

Each serialized record is limited to 64 KiB; a capped writer stops serialization at the boundary instead of first allocating an unbounded output buffer. Keys are limited to 4096 UTF-8 bytes and cannot be empty or contain NUL; records allow at most 64 evidence items, 1024 bytes per provider and 8192 bytes per evidence summary. Nonfinite model output is rejected. Read queries check payload length before materializing it. Database-copy queries guard key/hash lengths before reading strings; manual hash validation compares in SQL rather than loading an arbitrary stored hash.

SQLite itself is a local-data component, not a cryptographic evidence boundary. No raw pixels, prompts fetched from remote URLs, thumbnails or network requests are created by this cache. Strings are data, not instructions. Clearing a record is a logical local deletion; no forensic secure-erasure claim is made about backups, filesystems or storage hardware.

## Schema and failure handling

The database is identified with application ID `GLAI` and schema version 2. Version 1 migration transactionally adds byte accounting and the LRU index while preserving automatic payloads and manual decisions. Unknown application IDs, incomplete known schemas, unsupported older versions, future versions and SQLite corruption return explicit errors.

An existing database is opened read-only for application-ID/version/shape and integrity checks before any migration or mutation. A failed preflight does not rename, delete or overwrite the original database. Callers may continue with an explicitly disclosed in-memory fallback, leaving manual recovery possible. Symbolic-link targets and non-regular endpoint paths are rejected. A nonexistent or zero-length explicitly supplied file may be initialized, and its application-data parent directory is created if needed.

SQLite busy waiting is bounded at 0 ms per lock wait. Each operation also installs a SQLite progress callback that interrupts after approximately 25 ms of SQL VM time or 500,000 VM instructions; a later operation gets a fresh budget. SQLite limits cap string/blob/row size at approximately 70 KiB, SQL length at 16 KiB, and query shape; attached databases, triggers and auxiliary SQLite workers are disabled. These callbacks cannot interrupt a blocked filesystem call, so they are not a hard wall-clock I/O bound. Integrity checking, schema migration and LRU enforcement remain synchronous; the caller must keep them off the interactive rendering path. Total elapsed time and I/O under very large or malicious databases require platform/resource validation. Linux unit tests do not establish Windows upgrade, disk-full, crash-recovery, power-loss or native-UI latency acceptance.

## Automated coverage

Inline tests cover precedence and phase exhaustiveness; high-model/capture and low-model/trusted-generation combinations; manual/source conflicts and staleness; unsigned metadata plus signature failure; untrusted/compatible claims; absent/unapproved/invalid/out-of-scope models; filter sums and suspected exclusion; content hashes and stable identities; model-only invalidation; persistent round trips; old content isolation; LRU/manual separation; zero budget; durable manual decisions while automatic persistence is off; clear/delete through persistence toggles; schema migration; foreign/future/corrupt database preservation; malformed schemas/payloads; record-size bounds; oversized stored keys/hashes; SQLite value and SQL-work limits; recovery after an interrupted query; application-data directory creation; approval deauthorization; and source-file nonmodification.

Final test execution, platform gates and runtime integration are reported separately in the release-validation record. An implementation test fixture with an approved model flag is not a real approved detector.
