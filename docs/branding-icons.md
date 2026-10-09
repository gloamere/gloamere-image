# gloamere-image branding and Windows resources

Design recorded before implementation, 2026-10-09.

## Identity and geometry

The exact public product name is `gloamere-image`. Its original repository-native
mark combines a rounded deep-teal tile, an open image frame, a low horizon and an
amber setting sun. The horizon continues into the right-hand frame edge. The
asymmetric opening and short amber accent make the silhouette recognizable without
letters, copied logos, fonts, stock art or a photorealistic thumbnail.

- Deep teal `#113E42`: stable outer tile and dark-background definition.
- Quiet teal `#4D8D89`: large lower plane, kept subordinate to the image outline.
- Pale mist `#E2ECE5`: image frame and foreground horizon for strong contrast.
- Warm amber `#EEB866`: focal sun and the small frame opening.
- Master view box: 64 × 64. Tile begins at 2 and ends at 62. The image outline uses
  broad rounded strokes and clear internal space. No detail depends on text.
- Transparent corners; no baked white or black backdrop. Inspect on both light
  and dark backgrounds, at actual 16, 20, 24, 32, 40, 48, 64, 128 and 256 pixels.

`assets/gloamere-image.svg` is the editable source. A checked-in generator renders
each size independently with installed Inkscape and uses Pillow to make RGBA PNGs
and one multi-resolution ICO. Rendering is a maintainer operation; no SVG engine,
Python, Inkscape or new crate is required when building or running the app.

## Integration boundaries

- `assets/gloamere-image.ico`: Windows EXE resources and installer/uninstaller.
- `assets/gloamere-image-64.png`: `egui::ViewportBuilder::with_icon`, decoded once
  with the existing `image` crate. This covers the window/taskbar separately from
  the PE shell icon; it must not silently retain eframe's default logo.
- Main binary: `gloamere-image.exe`. Helper: `gloamere-image-worker.exe`.
- Existing Cargo package/library identities may stay unchanged. Both binary
  targets have explicit new `[[bin]]` names.
- Action glyphs continue to use the existing hand-drawn vector approach in
  `src/viewer_ui.rs`. Branding assets are not a font-icon dependency.

## Native Windows resource compilation

Both build scripts share `build-support/windows_resources.rs`. Resource decisions
use Cargo's `TARGET`, never host-only `cfg!(windows)`. On a Windows/MSVC target the
script emits `/Brepro` and uses Microsoft's Windows SDK `rc.exe` from the initialized
VS developer environment. `RC` can identify an explicit SDK compiler executable.
The script generates an ASCII `.rc` in `OUT_DIR`, copies the ICO beside it, and
compiles with `/fo`. No absolute asset paths are embedded in the script or PE.

The resource script defines icon group ID 1 with all nine sizes and VERSIONINFO ID
1. ProductName is exactly `gloamere-image`; FileDescription, InternalName and
OriginalFilename distinguish the main executable and worker. Package version
`0.4.0` becomes fixed version `0,4,0,0`. An SDK failure, missing/empty output or
missing icon is a hard build failure. No timestamp, updater, service, new runtime
crate or binary-size-sensitive dependency is introduced.

### Cloud-only source check

This Linux executor has no Windows SDK or native Windows linker. Cross-target
source validation is explicitly opt-in:

```sh
GLOAMERE_WINDOWS_RESOURCE_CHECK_ONLY=1 cargo check --workspace --all-targets \
  --locked --target x86_64-pc-windows-msvc
```

The override is accepted only for a non-Windows host, Windows/MSVC target and the
debug profile. It emits a clear warning plus a deliberately nonexistent resource
link input. `cargo check` does not link; an attempted executable link fails rather
than silently omitting branding. The override is rejected in release or native
Windows builds. This is not resource compilation, native execution or proof that
Explorer/Start menu/taskbar/default-app icons refresh correctly.

## Verification plan

1. Regenerate assets and inspect a contact sheet on light/dark backgrounds.
2. Validate PNG dimensions/mode/alpha and all nine ICO directory entries; ensure
   the packaged frame bytes match the corresponding PNG frames.
3. Unit-test resource metadata generation, version limits and check-only policy.
4. Run Cargo's Windows-target source check with the explicit override.
5. On Windows, build both EXEs through the existing SDK/VS initialization; inspect
   RT_GROUP_ICON (14), RT_ICON (3), RT_VERSION (16), original filenames and version
   strings. Confirm native icon loading and Start/taskbar/file-association behavior
   at 100%, 125%, 150% and 200% DPI. Native assertions remain pending in the cloud.

## Official references

- [Microsoft: ICON resource](https://learn.microsoft.com/en-us/windows/win32/menurc/icon-resource)
- [Microsoft: VERSIONINFO resource](https://learn.microsoft.com/en-us/windows/win32/menurc/versioninfo-resource)
- [Microsoft: resource types](https://learn.microsoft.com/en-us/windows/win32/menurc/resource-types)
- [Microsoft: RC command line](https://learn.microsoft.com/en-us/windows/win32/menurc/using-rc-the-rc-command-line-)
- [Cargo: build scripts and per-binary linker arguments](https://doc.rust-lang.org/cargo/reference/build-scripts.html)

## Implemented checks and current evidence (2026-10-09)

- Independently rendered all nine PNG dimensions with Inkscape 1.4; RGBA mode,
  transparent corners, opaque interior and exact ICO-frame pixels passed.
- Inspected the generated light/dark sheet including actual 16px and enlarged
  16px detail. The image frame and warm upper-right focal point remain legible.
- ICO size: 20,650 bytes. The embedded 64px PNG is 1,825 bytes. Neither adds a
  runtime dependency. Two complete asset-generation runs were byte-identical.
- Three Rust unit tests pass: exact main/worker metadata, version validation and
  check-only restrictions. Four actual build-script invocations additionally
  exercise permitted debug cross-check, forbidden release override, forbidden
  Windows-host override, and forbidden implicit cross-host resource omission.
  The allowed case also used an OUT_DIR containing spaces and Chinese characters;
  this tests Cargo output construction only, not SDK compilation in that path.
- Five Python tests pass for the standalone PE checker, using clearly synthetic
  PE fixtures: both executable identities, resource-tree round trip, incorrect
  icon, missing icon, wrong name/version, malformed version blocks and fake or
  truncated executables. Synthetic fixtures are not native execution evidence.

Native release gate (after building both actual executable files):

```sh
python build-support/verify_windows_resources.py --version 0.4.0 \
  target/release/gloamere-image.exe \
  target/release/gloamere-image-worker.exe
```

The checker parses the PE resource tree without executing either file. It requires
exact committed icon data at each resolution, one RT_GROUP_ICON ID 1, nine RT_ICON
records, and RT_VERSION ID 1 with correct fixed and string versions and all brand
fields. Failure exits nonzero. Its icon path can be overridden with `--icon`.

Still required on Windows: official rc.exe compilation and linking (including a
source/output path containing spaces and Chinese characters), this native PE gate,
icon appearance at the four DPI scales, cache-refresh/shortcut/default-app checks,
and actual installation/upgrade/uninstallation acceptance. Linux validation must
never be reported as completion of these native checks.

Additional official implementation references:

- [Microsoft: PE format and resource directories](https://learn.microsoft.com/en-us/windows/win32/debug/pe-format)
- [Microsoft: VS_VERSIONINFO structure](https://learn.microsoft.com/en-us/windows/win32/menurc/vs-versioninfo)
- [Microsoft: VS_FIXEDFILEINFO structure](https://learn.microsoft.com/en-us/windows/win32/api/verrsrc/ns-verrsrc-vs_fixedfileinfo)
- [Inkscape: command-line use](https://wiki.inkscape.org/wiki/Using_the_Command_Line)
- [Pillow: ICO and PNG formats](https://pillow.readthedocs.io/en/stable/handbook/image-file-formats.html)

## Taskbar identity

The process calls `SetCurrentProcessExplicitAppUserModelID` before any UI, with
`LightImageTool.Image`, matching the installer's shortcut AppUserModelID. This
retained opaque identity is separate from the visible product name and does not
change file defaults or user pin directories. An API error is logged without
preventing image viewing. Native QA must check grouping and previously pinned
legacy shortcuts; the installer does not promise to rewrite user-managed pins.
Official reference: https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-setcurrentprocessexplicitappusermodelid
