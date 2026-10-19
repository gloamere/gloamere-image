# Deliberately unprovisioned

No production root or public bundle is present. A separately authorized build may copy only reviewed `public-bundle.json` and signed `root.json` here, then explicitly change `COMPILED_PUBLIC_BUNDLE` in `src/bootstrap.rs` to compile them with `include_bytes!`.

Do not copy encrypted or plaintext private-key files, passphrases, TLS/ACME credentials, or a complete key-vault directory. Do not modify the current candidate to enable updates without reviewing the public root fingerprint, exact build pins, trusted IP HTTPS deployment and persisted-state migration implications. See `docs/PUBLIC-BOOTSTRAP-DESIGN.md`.
