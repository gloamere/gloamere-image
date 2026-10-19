# v0.5 explicit clipboard adapter

Status: implementation and synthetic tests; Windows native interoperability and UI acceptance remain required. This document does not claim a release pass or macOS support.

## Interface and source semantics

`ClipboardAdapter::read_image_or_file`, `copy_pixels` / `copy_pixels_cancellable`, and `copy_text` are explicit one-shot operations. All adapter instances share a nonblocking process lock on Windows. They install no listener, persist no history, and do not retry or take the OS clipboard lock from another owner. Text fields and modal keyboard routing are the application layer's responsibility.

Read order is registered `PNG`, `CF_DIBV5`, `CF_DIB`, one Unicode `CF_HDROP` file reference, then Unicode text containing an unambiguous local image path. An advertised higher-priority pixel representation that is malformed, unsupported or oversized fails explicitly; it does not silently fall through to unrelated text or files. A second file reference requests selection. Legacy ANSI file references are outside this narrow first implementation.

Pixel results contain RGBA, a representation-only `ClipboardSource`, color status and warnings. They have no invented original filename, original hash, author, signature or provenance evidence. A copied image contains pixels only, so an original valid signature is not inherited. PNG color and orientation metadata can be used to normalize pixels, but are not presented as proof of the original file. The caller creates a temporary clipboard document and disables original-file provenance actions. A real local file reference goes through normal bounded image opening.

## Bounds and allocation order

- Before application-owned clipboard copies, `GlobalSize` must be nonzero and at most 64 MiB for image representations.
- The fixed PNG signature/IHDR or narrow DIB header is checked before snapshot allocation: nonzero dimensions, at most 16,000,000 pixels, each side at most 32768, checked row/offset/image lengths.
- Unicode text is limited to 32767 UTF-16 units plus its terminator. Unicode DROPFILES is bounded to the same single-path budget and a small header/terminator allowance.
- Decoder scratch and owned snapshots/pixels use the shared admission budget. PNG's existing locked `png` 0.18.1 crate is now a direct dependency, permitting borrowed ICC inspection before profile parsing, ignoring unrelated text metadata, and explicit output-buffer sizing. The lockfile adds no different PNG version.
- PNG uses a 128 MiB decoder limit plus separately reserved raw/conversion/orientation working buffers. ICC profiles exceeding the existing 4 MiB application cap produce an unmanaged-color warning without cloning that profile. PNG library limits are best effort, not a hard process sandbox.
- No large input is passed first to an allocating `arboard::get_image`. The existing arboard dependency remains transitive through egui; this adapter does not add a second clipboard framework.

`GetClipboardData` can itself request delayed rendering or OS format synthesis. The application cannot preflight allocations made by Windows or the source application before a handle exists. OS calls and synchronous decode are not guaranteed to be cancellable. Input validation is an application safety boundary, not an OS memory sandbox.

## Narrow DIB support and transparency

Accepted headers are exactly 40-byte BITMAPINFOHEADER and 124-byte BITMAPV5HEADER. Supported pixels are 24-bit/32-bit uncompressed BI_RGB and 32-bit BI_BITFIELDS using the standard byte-aligned RGB masks. The adapter checks plane count, palette absence, image size, stride including 24-bit row padding, signed top-down/bottom-up height, and every offset before access. It does not enable BMP file parsing, arbitrary masks, indexed palettes, RLE/JPEG/PNG-compressed DIB, or embedded/linked color profiles.

An explicit standard alpha mask in DIBV5 retains straight RGBA alpha. A narrow compatibility case recognizes 32-bit BI_RGB DIBV5 with `alphaMask=0xff000000` and RGB masks either all zero or the exact standard masks, matching the Chromium/Electron pattern described by pinned arboard 3.6.1. This is covered by a named synthetic fixture, not a claim that a particular browser version has been tested. Ordinary BI_RGB without an explicit V5 alpha mask treats its reserved byte as opaque and reports that limitation. It never heuristically treats all-zero reserved bytes as alpha.

DIBV5 sRGB/Windows-color-space tags are accepted. Empty calibrated-color metadata is treated as unknown and visibly assumed sRGB. Nonempty calibrated RGB and linked/embedded profiles are rejected; no profile filename is opened.

Copy prepares bounded PNG and bottom-up 32-bit DIBV5 with standard RGBA masks before opening or emptying the clipboard. Both represent the complete supplied image; there is no checkerboard, toolbar, viewport crop, or background flattening. PNG/DIBV5 contain no retained EXIF, source ICC, XMP or signature. Native interoperability still needs verification, especially target applications that ignore alpha. Exporting a PNG copy is the fallback for an incompatible target.

## Local paths and filesystem boundary

Path syntax must be a drive-absolute Windows JPEG/PNG/GIF/WebP path. The parser rejects URLs, UNC/device paths, alternate streams, relative paths, dot traversal, control characters, wildcard syntax and DOS device names. Windows rejects mapped network or unknown drives, then checks ancestors before their children and rejects reparse points and offline/cloud-placeholder files; it checks regular-file status and the 64 MiB file cap. This conservatively rejects otherwise legitimate junction/symlink paths rather than fetching them.

The returned path is rechecked by the normal loader. Like ordinary path-based opening, this does not provide race-free protection against an adversarial process changing filesystem components between checks. No explicit networking, URL download, shell execution or cloud hydration is implemented.

## Win32 ownership and failure semantics

Read handles remain borrowed while the clipboard is open, are locked only for bounded inspection/copy, are unlocked before `CloseClipboard`, and are never freed by the application. The system clipboard closes before decoding or local filesystem checks.

Copy allocates moveable global blocks and fills/unlocks every representation before the one required `EmptyClipboard`. A valid invisible message-only window supplies clipboard ownership; no NULL-owner write is attempted. Each successful `SetClipboardData` transfers only that handle to Windows. RAII frees untransferred handles and closes the clipboard/window on failure. A failure after replacement starts, including second-format publication or final clipboard close, returns `WriteFailed` and warns that old contents may already have changed. There is no false success, automatic clear on preparation failure, or promise of transactional rollback.

The worker-facing `copy_pixels_cancellable` checks cancellation before and between preparation stages. Immediately before `EmptyClipboard`, the shared `Cancellation::publish` atomic gate chooses the winner: cancellation first means no clipboard mutation; publication first means actual success or partial OS failure is returned even if Cancel arrives afterward. The convenience `copy_pixels` uses a fresh token. Pure deterministic tests cover both winners and preservation of partial-write errors. PNG encoding and synchronous OS calls still do not promise immediate interruption.

Cancelled, busy, no supported format, over-limit, unsupported platform, malformed/unsupported data, multiple files, nonlocal paths, budget failure and system failures remain distinct errors. Windows contention can be retried by another explicit user action. Non-Windows platforms report unimplemented/unverified support; they do not report successful no-ops.

## Source validation performed

On 2026-10-09, the cloud Linux core harness passed all 25 focused clipboard tests, including alpha/ICC/EXIF/palette conversion, malformed/over-limit dimensions, narrow DIB layouts, path/file-list parsing and the cancellation publication gate. The Windows MSVC target library passed source-only `cargo check` and `cargo clippy -- -D warnings` with `GLOAMERE_WINDOWS_RESOURCE_CHECK_ONLY=1`. This mode intentionally does not compile native Windows resources, link an EXE or access a Windows clipboard. The ordinary Linux product manifest has no supported native GUI backend; the core harness supplies its own testing backend without changing product platform claims.

## Required native acceptance, not yet performed

Record OS build, app versions, EXE hash, image fixture hashes and the exact direction of each transfer:

1. Windows Snipping Tool, Edge, Chrome and one actual chat client, each into/out of gloamere-image.
2. Transparent PNG with alpha values 0/1/128/255 and colored transparent-edge pixels. Check ordinary paste and DIBV5-only inputs, including the BI_RGB compatibility case.
3. 24-bit padded rows, top-down/bottom-up DIB, opaque 32-bit DIB, malformed masks/headers, truncated data and unsupported profiles.
4. Empty clipboard, clipboard held by another process, repeated explicit reads, oversized global block, dimensions beyond each limit, bad PNG/APNG, valid/bad/oversized ICC.
5. Single Chinese/emoji local path and file reference, multiple files, plain text, URL/UNC/mapped drive, junction, symlink and offline/cloud placeholder.
6. Edited/rotated/cropped complete image and current composited animation frame copied without UI/background, followed by target-side alpha verification.
7. Text and numeric focus retain normal Ctrl+C/V, modal dialogs block image paste, unexported edits trigger the existing guard, and clipboard errors leave the current document intact.
8. Preparation/allocation failure leaves old contents alone; simulated native publication failure is not shown as success. Verify pasted content after copy and after the sender window/application closes.

## Reviewed references

- [Microsoft standard clipboard formats](https://learn.microsoft.com/en-us/windows/win32/dataxchg/standard-clipboard-formats)
- [GetClipboardData ownership and untrusted input](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getclipboarddata)
- [SetClipboardData transfer and NULL-owner warning](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setclipboarddata)
- [GlobalSize allocation length](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-globalsize)
- [BITMAPV5HEADER layout, masks and color fields](https://learn.microsoft.com/en-us/windows/win32/api/wingdi/ns-wingdi-bitmapv5header)
- [GetDriveTypeW local/network classification](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getdrivetypew)
- [PNG 0.18.1 decoder and best-effort limits](https://docs.rs/png/0.18.1/png/struct.Decoder.html)
- [Pinned arboard 3.6.1 Windows implementation](https://docs.rs/crate/arboard/3.6.1/source/src/platform/windows.rs)
