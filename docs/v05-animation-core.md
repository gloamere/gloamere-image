# v0.5 GIF / WebP core and verification

## Scope and pinned dependencies

The input pipeline now content-detects JPEG, PNG, GIF and WebP. Existing static JPEG/PNG orientation, ICC, gray-profile, alpha and export behavior stays on the existing pipeline. Static GIF and WebP use that same path. APNG stays explicitly rejected. No GIF/WebP encoder or animation-saving user feature was added.

`image` remains exactly 0.25.10, with `default-features = false` and `jpeg`, `png`, `gif`, `webp`. Two direct dependencies expose the matching underlying codecs already introduced by those features: `gif = 0.14.2` and `image-webp = 0.2.4`. The lockfile adds only `color_quant 1.1.0`, `gif 0.14.2`, and `image-webp 0.2.4`; `weezl` was already present. These packages use permissive licenses; the packaging notice inventory must include them.

Official reference material, reviewed against the exact locally downloaded package source:

- [image AnimationDecoder](https://docs.rs/image/latest/image/trait.AnimationDecoder.html)
- [gif 0.14.2 decoder source](https://docs.rs/crate/gif/0.14.2/source/src/reader/mod.rs)
- [image-webp 0.2.4 decoder source](https://docs.rs/crate/image-webp/0.2.4/source/src/decoder.rs)
- [image-webp 0.2.4 compositor source](https://docs.rs/crate/image-webp/0.2.4/source/src/extended.rs)
- [WebP container specification](https://developers.google.com/speed/webp/docs/riff_container)
- [image Limits contract](https://docs.rs/image/latest/image/struct.Limits.html)

## Why the animation wrappers are not used unmodified

Exact-source audit and pixel tests found four important distinctions:

1. `image`'s GIF `loop_count()` maps a GIF decoder's default `Repeat::Finite(0)` to infinite and does not convert a finite extension's extra-repeat count to total traversals. This application uses the direct codec value: no extension means one traversal, finite `n` means `n + 1` traversals, and the explicit infinite value remains infinite. WebP `Times(n)` already means total traversals.
2. GIF's image frame iterator returns composited RGBA canvases, but its wrapper does not expose all admission/header options. The application uses the same small, tested disposal operation with direct `gif` decoding: transparent source pixels preserve prior pixels; Keep/Any persists output; Background clears only the local rectangle to transparent; Previous leaves the pre-frame canvas untouched. Source GIF background palette color is not painted behind transparency, matching common viewers.
3. Pinned `image-webp`'s animated compositor has disposal branches keyed to the current frame's dimensions/color type. Its RGB clear branch can use a three-byte stride on an RGBA canvas, and a full current frame can clear more than the previous frame's disposal rectangle. Its approximate blend arithmetic can differ by a channel value. The application therefore decodes individual VP8/VP8L frame payloads with the codec and performs exact rounded unassociated-alpha source-over and previous-rectangle disposal itself. Each traversal starts from transparent. The WebP ANIM background hint is deliberately not painted; this policy is consistent for start and disposal.
4. `image-webp` can allocate VP8 YUV planes from the compressed keyframe dimensions before checking them against ANMF dimensions. Its memory-limit setting covers metadata but does not hard-limit all codec allocations. A narrow checked RIFF/ANMF framing guard validates rectangles and inner VP8/VP8L dimensions before any pixel decode, including ALPH+VP8 and extended static WebP. Duplicate/out-of-order VP8X, ANIM and duplicate bitstream ambiguity fail closed. A virtual standalone WebP reader exposes each frame's original snapshot range under a tiny synthesized RIFF/VP8X header, without copying compressed payloads.

This is not a custom VP8, VP8L or GIF LZW decoder. Compression remains in the pinned official codecs. The compositor is row-cancellable and covered by exact pixel goldens, including real VP8 and ALPH+VP8 payloads.

## Bounds and ownership

- Input file/snapshot: 64 MiB maximum, unchanged. The immutable `Arc<Snapshot>` is reused for the source and every traversal; playback never reopens the path. File replacement after opening cannot change later frames.
- Static image: unchanged 16,000,000 pixels and 32,768 single-axis cap.
- Animated canvas: 4,000,000 pixels and the same single-axis cap. A GIF first-frame probe uses one reusable row before deciding whether it has a second frame; therefore a one-frame GIF can still use the static 16 MP allowance. Animated inputs exceeding 4 MP fail to open with an explicit safe-limit message. They do not silently appear to be fully supported static images, and no over-limit first-frame preview is attempted.
- Traversal: at most 2,000 decoded frames. Exactly 2,000 frames may end or loop normally. A 2,001st frame pauses with an explicit limit error; the UI's last valid frame remains available. WebP stores at most 2,001 small frame descriptors, never decoded-frame arrays. GIF advances sequentially.
- Application animation admission: 128 MiB per source. A local budget includes metadata allowance, conservative decoder/compositor/normalization working space, compressed-frame scratch, and current/prepared pixel owners. Working admission is `5 × RGBA canvas bytes + 8 MiB + 2 × largest admitted WebP frame payload`. Output pixels are charged separately. Admission can reject a high-complexity/noisy input even below the 4 MP canvas limit.
- A weak-owner ledger observes actual `Arc<Pixels>` ownership, so extracting/cloning a pixels reference cannot silently disappear from accounting while that source continues decoding. The supported worker flow keeps only the displayed frame and at most one prepared frame. Keeping two old outputs prevents a third output instead of creating an accidental animation cache.
- Global 768 MiB operation reservations also cover the source metadata, working buffers, immutable snapshot and actual owned output capacities. Animation admission does not replace the global budget. UI/GPU texture reservations remain the application's responsibility; textures are reused for the current frame.
- GIF metadata is capped at 4 MiB per codec ICC/XMP vector. Header admission temporarily allows 16 MiB for those vectors and a profile copy, then shrinks to the persistent allowance. `read_into_buffer` uses application-sized frame buffers independently of that metadata limit; an actual 4 MP GIF regression verifies this.

The limits are application admission and owned-buffer accounting, not a hard process-memory sandbox. In particular, pinned WebP lossless Huffman/entropy tables can amplify small compressed input and do not all honor the codec memory-limit API; allocator overhead, GPU/driver copies, CMS internals and OS activity are not a claim about process-private memory. The narrow dimension guard closes the concrete oversized-VP8 mismatch; it does not claim complete malicious-input isolation. Process sandboxing would be a separate hardening project.

## Normalization, failures and timing

The source reads EXIF orientation and ICC once. Composited full canvases are oriented once, then use one reusable RGB-to-sRGB transform per source. Alpha is preserved byte-for-byte. Invalid/unreadable/unsupported ICC is distinctly marked unverified; missing ICC is explicitly assumed sRGB. A gray ICC on already-RGB GIF/WebP is not silently treated as valid original gray encoding. Static JPEG/PNG retain their existing original-gray normalization path.

Every delay below 20 ms, including zero, is consistently raised to 20 ms. The source warning discloses this policy and each frame reports `delay_normalized`. Ordinary delays retain their value. `PlaybackClock` accumulates monotonic deadlines rather than adding each delay to the latest wake time. Pause/resume rebases the deadline; it does not catch up missed time. More than two seconds of sustained lateness asks the UI to pause, rather than enter an unbounded catch-up loop.

Errors and cancellation never mutate an already returned frame. The stream stops on malformed payload, traversal limit or failed normalization. Worker boundaries catch codec panics and return a pause/error result. Cancellation is checked during snapshot reads, framing walks, frame boundaries, composition rows, color conversion rows and publication. A synchronous codec call or OS operation is still cooperatively interruptible only where the underlying implementation reads/checks; no instant-interruption claim is made.

## Application integration contract

`LoadedImage.animation` is `None` for static inputs and an `Arc<AnimationSource>` for animated inputs; `LoadedImage.pixels` still contains the correct first frame. Source fields expose first delay, canvas dimensions, format, loops, color status and warnings.

`AnimationWorker` owns one OS thread, one replaceable pending demand and one result slot. An `AnimationRequest` carries `generation`, `request_id`, source, cancellation token and restart flag. A new non-restart request reconstructs/discards the already-displayed first frame, then returns exactly the next frame. Restart returns frame one. Following requests return exactly one frame; `Ok(None)` is finite completion. No request means no background decoding. Keep at most one request in flight and filter both IDs before presentation. Update the loaded/current pixel owner when displaying a new frame; do not keep an obsolete first-frame owner indefinitely.

Use a fresh cancellation token after stop/restart; it may otherwise be reused throughout a session. The worker's stop operation targets the token actually retained by the active decoder, including if a later request supplied a different token. Stop is nonblocking, cancels active/pending work, clears stale output and eventually drops the stream. `shutdown` joins the owner. The worker neither retains egui Context nor invokes repaint during join, avoiding the known egui-memory-lock shutdown deadlock pattern. The app schedules polling while a request is active and frame wakeups only while playback is active.

## Verification commands and evidence boundary

The `tests/animation.rs` suite has 19 ordinary tests and one ignored 30-minute real-time soak. It covers:

- GIF Keep/Any-equivalent behavior, Previous, transparent Background, partial rectangles, transparent pixels, zero delays and finite/infinite loop counts.
- WebP lossless, actual VP8, actual ALPH+VP8, alpha blend/replace, previous-rectangle disposal before a full current frame, and loop-canvas reset.
- Animated EXIF normalization, all eight static WebP EXIF orientations, valid RGB ICC, invalid ICC and alpha preservation.
- Static single-frame GIF, static WebP, a static GIF above 4 MP, actual 4 MP animated GIF admission.
- Both GIF and WebP exact 2,000/2,001 frame boundaries, later corrupt payloads, malformed inner dimensions, duplicate-header ambiguity, held-frame budget refusal and cancellation.
- Monotonic deadline drift, pause/resume, worker step/restart/end/generation behavior, and 100 rapid generation replacements.

Cloud Linux execution:

```sh
# CLOUD_HARNESS points to a separate developer-only Linux wrapper that includes
# the actual product sources, enabling x11/portal only for headless cloud tests.
cargo test --manifest-path "$CLOUD_HARNESS/Cargo.toml" --test animation_core --offline
cargo clippy --manifest-path "$CLOUD_HARNESS/Cargo.toml" --test animation_core --offline -- -D warnings
GLOAMERE_WINDOWS_RESOURCE_CHECK_ONLY=1 cargo clippy --target x86_64-pc-windows-msvc --lib --offline -- -D warnings
```

Native Windows can run the same core suite via `cargo test --test animation`; that command itself is not native evidence until it is actually run there.

Explicit real-time soak (30 minutes by default; setting `GLOAMERE_ANIMATION_SOAK_SECONDS=3` requests only a short smoke):

```sh
cargo test --test animation realtime_animation_soak_30_minutes -- --ignored --nocapture
```

In the Linux harness replace the target name with `animation_core` and add its manifest path. The soak checks two looping 64×64 synthetic GIF/WebP sources, exact pixel colors, sustained owned-reservation bounds and clock deadlines, and reports frames more than 5 ms late. It uses no GPU, egui scheduler or Windows APIs. A passing core soak cannot establish whole-process memory, GPU memory, user-visible frame timing, minimization behavior or native Windows stability. Those remain native acceptance gates.

As of the implementation checkpoint, 19 ordinary core tests and strict library/test clippy checks pass. A 3-second soak smoke produced 60 displayed frames per codec, no >5 ms late frames, and unchanged global owned reservations of 33,719,171 bytes. The completed full 30-minute run is reported separately below; its result is not inferred from this smoke test.


### Completed 30-minute core run (2026-10-09)

The retry ran continuously from 07:26:30 to 07:56:30 UTC and exited successfully: 36,000 GIF frames and 36,000 WebP frames over exactly 1,800,000 ms. Every recorded global owned-reservation sample was 33,719,171 bytes. The two animation-source reservation readings stayed 16,875,520 and 16,875,848 bytes. Frames delivered more than 5 ms late were 404/36,000 (1.122%) for GIF and 548/36,000 (1.522%) for WebP. Both final normalized timelines ended at 1,800,000 ms. Lateness ratio is separate from cumulative timing error; these are synthetic core results on a shared cloud machine, not native GUI performance.

An earlier environment-interrupted run has only 540 seconds of evidence. It is retained as incomplete and is not counted as a 30-minute pass.

Verified start/final source hashes for the completed retry:

- `src/animation.rs`: `b5c9c25a33e08a90a54cf3c2392e0de270a958a1a4b32fa0a74ef702bf364339`
- `src/pipeline.rs`: `323f32d0649e1ed60bfd80fbbe09696231621f5a7d21fd744c8197194c69084c`
- `tests/animation.rs`: `30a8747109322616980dad369c675985c68e6e62a516f928fa48287790069fdf`
- Linux test executable SHA-256: `e667115c9750c6954e229e5fbfc5f8f01065aa755d6447916766fdf047041b24`

The executable's modification time precedes the run; its checksum and all three source checksums matched again at completion. The evidence directory contains the raw retry log, executable/source checksums and `animation-soak-retry-provenance.json`. Full process/GPU memory and Windows GUI soak remain separate pending acceptance work.
