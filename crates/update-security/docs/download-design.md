# HTTPS transport and artifact download addendum

Design recorded before implementation, 2026-10-10. Scope: `src/download.rs`.

## Trust boundary and API

`HttpsPolicy` requires an explicit, nonempty HTTPS origin allowlist. There is no production default, hostname guess, HTTP/file fallback, embedded production key, TLS verification bypass, or installer execution. Only configured exact origins (including port) are permitted. The policy rejects credentials, queries, fragments, escaped path delimiters, backslashes, dot path segments, and malformed URLs before URL-parser normalization where a raw string is available. The owning metadata layer must continue validating signed target names before converting them to `Url`.

`HttpsTransport` implements the actual `tough::Transport` interface for metadata. It collects only bounded, streaming-read metadata and returns a bounded byte stream to tough, which performs TUF verification. `Downloader` streams artifacts to local files and releases a completed path only after exact signed length and SHA-256 verification. Expected target values must come from verified TUF metadata, never from HTTP headers or resume sidecars.

## Network behavior

Use reqwest's Rustls client with certificate/hostname validation, `https_only`, redirects disabled, decompression explicitly disabled, `Accept-Encoding: identity`, system proxy discovery and native trusted certificate roots, and connect/read/request timeouts. Redirect handling is manual, bounded, and applies full URL/origin checks to every hop, including relative Location values before resolution. An overall deadline includes all hops and reads. Cancellation is checked while awaiting the response, each body chunk, and between local I/O operations. Local writes are drained before observing cancellation, preventing background writes from outliving the lock. HTML MIME types and an HTML body prefix are rejected; any content encoding except identity is rejected. Duplicate/invalid security-relevant headers, unsolicited partial responses, overflows, early EOF, and inconsistent advertised lengths fail closed.

## Resume protocol and local state

Only `<destination>.part` plus a bounded JSON sidecar are resumable. The sidecar binds the original URL, signed byte length, signed SHA-256, and a syntactically strong ETag. Existing bytes are rehashed before use. A resume request sends `Range: bytes=<length>-` and `If-Range: <etag>`; a 206 response must repeat the same strong ETag and an exact Content-Range from the existing length through the final expected byte, with the signed total. A 200 response resets the partial and begins afresh. A changed/missing ETag or invalid range on a 206 response invalidates the partial and fails, so the next invocation starts afresh. No mixed-object result can be promoted. Partial overflow, wrong final hash, HTML, and malformed range are removed; network interruption/cancellation can preserve only strongly bound partials. Completion flushes and syncs the file before promotion and deletes its sidecar.

A per-destination nonblocking OS file lock (released automatically on process exit) prevents cooperating concurrent writers. Only absolute local destinations are accepted. Every existing directory ancestor and final/temporary file is checked for symlink/reparse substitution before local mutations, cleanup, and promotion. Unix cache parents must belong to the effective user and disallow all group/other permissions; writable nonsticky ancestors are rejected. The Windows platform layer must verify current-user ownership and private ACL protection before calling the core. Existing nonregular temporary files are rejected. Callers must use a private, trusted local cache directory; this module does not defend against an administrator or an attacker with simultaneous write access to that directory. No shell, interpreter, remote command, or executable installer is invoked.

## Offline verification

The HTTP boundary is private and injectable only from this module's tests. Production construction always uses the real certificate-validating reqwest client. Tests use deterministic in-memory responses, without listening sockets, test-only TLS bypasses, or production trust roots. Coverage includes path/origin policy, redirect downgrade, redirect limits, HTML and encodings, exact length/hash, partial overflow, wrong Content-Range, strong/changed ETags, 200 restart, cancellation, and interrupted/resumed bytes.

## Primary references consulted

- tough 0.24.0 transport source and public exports: https://docs.rs/tough/latest/src/tough/transport.rs.html and https://docs.rs/tough/latest/tough/
- reqwest ClientBuilder: https://docs.rs/reqwest/latest/reqwest/struct.ClientBuilder.html (redirect policy, HTTPS-only, disabling decompression, connect/read/total timeout behavior).

- Rust standard-library OS file locks: https://doc.rust-lang.org/std/fs/struct.File.html#method.try_lock

Implementation review addendum: system proxy discovery and native trust roots are explicitly enabled through reqwest features; no extra trust certificates or certificate-validation bypass is exposed. Same-user directory races remain outside this integrity-only module's threat boundary; the installer must rehash and hold its verified file lease.
