# v0.5 installer and candidate packaging contracts

Implementation/checkpoint: 2026-10-09. This is source-level and synthetic-fixture
evidence only. No Inno compilation, Setup execution, actual registry integration,
Windows upgrade, or native lifecycle acceptance was performed in this cloud task.

## Implemented scope

- Keep one per-user unsigned Setup EXE and the exact existing AppId/OwnerId
  `9CCB6F43-7782-4F69-9E91-67C6D144737B`, install directory
  `{userpf}\LightImageTool`, owner marker `LIGHT-IMAGE-INSTALL.txt`, Explorer verb
  `LightImageTool`, and `LightImageTool.Image` ProgID/taskbar identity.
- Keep the branded `gloamere-image.exe` / `gloamere-image-worker.exe` pair and the
  existing app preferences path `%LOCALAPPDATA%\LightImageTool\settings.json`.
  Installer code does not read, replace, migrate, or delete preferences. Any app
  schema migration or invalid-file recovery is tested by the application suite.
- Register `.jpg`, `.jpeg`, `.png`, `.gif`, and `.webp` as supported candidate
  formats. The app description explicitly limits editing to static copies;
  this registration does not claim GIF/WebP encoding or native acceptance.
- Apply the optional Explorer task to all five extensions. Every format uses
  the same ownership preflight, install-boundary recheck, typed snapshot,
  tracked writes/deletions, rollback pruning, and uninstall protection.
- Keep extension defaults and UserChoice untouched. The Default Apps settings
  action remains unchecked, postinstall, one-time, and skipped for silent runs.
  No updater service, network check, new domain, or automatic default selection.

## Why predecessor profiles are separate

The old matcher checked exact registry trees, including value counts and label
text. Extending its only current profile would reject an otherwise valid v0.4
upgrade. `TDefaultAppProfile` now explicitly distinguishes:

1. `dapLegacy`: historical Light Image Tool label/path/icon, JPEG/PNG only.
2. `dapV04`: branded gloamere-image v0.4 label/path/icon, JPEG/PNG only.
3. `dapV05`: the same branded identity plus GIF/WebP and updated format labels.

Both predecessor profiles additionally require the owned existing installation.
Every accepted profile still requires complete exact `REG_SZ` values and one
matching discovery name. Partial/mixed profiles, unknown values/subkeys, foreign
owners, changed text/type, and duplicate discovery remain refusal conditions.
Legacy-branded Explorer menu profiles are accepted only for the three historical
extensions; they cannot grant ownership of GIF/WebP menu entries never shipped.

The v0.5 value list is a key/name superset of branded v0.4. Snapshotting it records
both old label text and absence of the new associations, so rollback can restore
v0.4 exactly. The old discovery-name entry is also included for earlier migration.
Concurrent changed values are preserved and reported; compare-before-write/delete
checks are not atomic transactions.

## Version and package evidence

`build-installer.py` derives the candidate version from both Cargo manifests and
requires them to match and fit Windows fixed-version bounds. Its `--print-version`
mode performs no build. The Windows release command uses this value in reports,
the paired PE resource gate, staged manifest, and unsigned Setup filename.
The installer fallback version is checked against the same candidate version.

Developer source archives must contain both Cargo manifests with the candidate
version. An older or mixed commit cannot be mislabeled with the current version.
The report also records animation core seconds per profile and labels runs shorter
than 1800 seconds as `short_smoke_only`; GUI playback remains `not_run`. Test logs
use `--nocapture` so the default 30-minute core soak reports progress each minute.
Dependency/license collection remains a separate gate. Staging excludes ordinary
JPEG/PNG/GIF/WebP image fixtures, screenshots, fonts, client source, and keys;
new-format support does not make image fixtures installation payload.

Linux checks run for this checkpoint:

- `python3 scripts/test-branding-migration.py`: 27 passed.
- `python3 scripts/test-default-apps-installer.py`: 17 passed.
- `python3 scripts/test-release-packaging.py`: 51 passed.
- `python3 build-support/test_windows_resources.py`: 5 passed; synthetic main and
  worker resources include both v0.4.0 and v0.5.0.
- `git diff --check`: no whitespace errors in this work.

`tests/fixtures/installer/branded-v0.4.json` freezes the predecessor identity,
typed candidate values, sample old preferences, non-owned UserChoice sentinels,
and ordinary-file names. Its tests are source contracts and independent Python
models, not an execution engine for Pascal or Windows APIs. Synthetic packaging
EXE bytes are fixtures, not usable Windows binaries.

## Native Windows acceptance still required

Bind every result below to the same candidate commit, actual EXE/worker/Setup
SHA-256 hashes, Windows version, and explicit passed/failed/not-run status.

1. Build the paired x64 MSVC Release binaries and compile with the approved
   official Inno Setup toolchain. Verify exact icons and v0.5 fixed/text versions
   on main, worker, and Setup. Preserve unsigned status and compiler evidence.
2. Fresh install, actual authorized v0.4 in-place upgrade, v0.5 repeat install,
   and optional older legacy fixture: one installed-app entry, same install
   path/ProgID, new paired binaries, unchanged existing preference file and user
   pictures. The isolated fixture does not substitute for the actual upgrade.
3. Repeat upgrade with Explorer checked and unchecked. Verify five correct
   quoted `--open "%1"` commands for Chinese, spaces, ampersands, and parentheses
   in paths. Verify GIF/WebP appear in Default Apps without choosing a default.
4. Snapshot all five extension default values and UserChoice before/after
   install, reinstallation, rollback, and uninstall. Confirm byte/type equality
   unless the tester separately makes a deliberate choice in Windows Settings.
5. Verify unchanged unchecked completion-page Settings choice, silent-run skip,
   Windows 10 generic Settings fallback, Windows 11 supported app-specific entry,
   and English/Chinese completion-page layout with the longer five-format text.
6. Running main/worker refusal, non-owned nonempty destination, changed/foreign
   menu or candidate key, mixed v0.4/v0.5 profile, changed string type, unknown
   values/subkeys, duplicate discovery, reparse paths, shortcut alias collisions,
   and moved existing installation must refuse before payload mutation.
7. Inject first/middle/last candidate/menu write failures, late file lock, and
   native cancellation. Restore exact v0.4 payload/manifest/uninstaller/shortcut
   hashes, old label text, and absence of GIF/WebP associations where originally
   absent. Preserve concurrent foreign changes and retained recovery evidence.
8. Upgrade then uninstall with ordinary JPEG/PNG/GIF/WebP files, preferences,
   unrelated text files, changed product values, and shared registry-parent
   sentinels present. Only verified owned payload/registration/shortcuts are
   removed; modified foreign values and ordinary files survive.

Do not report release acceptance from these source tests. Power loss, forced
termination, unavailable filesystems, and arbitrary concurrent replacement remain
separate failure boundaries inherited from the recovery design.

## Source handoff hygiene

The source archive includes only the five current v0.5 product/acceptance documents.
Historical delivery notes, execution instructions, task caches and screenshots are excluded;
product tests and their attributed/synthetic fixtures remain included for reproducibility.
A manifest records every delivered file and hash. The local cloud import commit does not
claim ancestry from an independently cleaned remote repository.
