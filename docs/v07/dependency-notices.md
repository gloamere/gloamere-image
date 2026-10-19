# Windows workspace notices — 2026-10-10

This inventory covers the locked Windows x64 normal/build dependency graph for the stable launcher, UI, provenance worker and update-security crate. It is a dependency-notice collection, not a legal opinion or a passing Windows build.

- 449 external crate versions; 823 bundled notice/declaration files.
- All 449 cached registry archives matched their SHA-256 checksums in the paired `Cargo.lock`.
- 772 direct published notice files matched bytes inside those locked archives.
- Previously checked-in upstream notices and declaration fallbacks passed the packaging script's recorded-hash verification.
- All 823 final notice/declaration file hashes passed `stage-installer-payload.notice_sources`.
- A fresh full-workspace collector run using this paired inventory as its offline cache completed with 449 packages, 819 ordinary notice files, four explicit declarations and no unresolved omissions. See `dependency-notices-recollection.txt`.
- Included additions cover `tough 0.24.0`, AWS-LC, native-trust reqwest transport, `zip 8.6.0`, `zopfli 0.8.3` and `zlib-rs 0.6.8`.

The four explicitly labeled published-license declaration fallbacks are `btree-range-map 0.7.2`, `range-traits 0.3.2`, `static-regular-grammar 2.0.2` and `typed-path 0.9.3`. The first three preserve the paired historical declarations and Apache-2.0 option. `typed-path 0.9.3` omits standalone licenses from its published archive; its exact published README license grant is retained, the Apache-2.0 option is selected, and full unmodified Apache-2.0 text is bundled beside it. Its README bytes were checked against the locked crate archive. These entries are not silently represented as published standalone license files or legal approval.

The repository-root `third-party-notices/inventory.json` is authoritative for installer staging. The paired notice tree contains exactly its 823 listed files plus `inventory.json`; no unlisted historical files or reports remain. `dependency-notices-verification.json` records the paired lock/inventory hashes and counts; `dependency-tree-windows.txt` records the actual active target graph. These reports live beside this document, outside the notice tree.

Reproduce collection with `cargo tree --workspace --locked --offline --target x86_64-pc-windows-msvc --edges normal,build --prefix none --format '{p}'`, then run `scripts/collect-licenses.py` with that tree, the local Cargo registry, repository-root `third-party-notices` as `--cache` and a fresh `--output`. The collector now discovers all explicitly listed root workspace members by their exact package name/version. Unsafe, globbed or inherited member definitions fail closed rather than silently skipping dependencies.

Packaging regression suite after this change: 54 tests passed. This Linux-safe suite does not compile a Windows executable or run an installer.
