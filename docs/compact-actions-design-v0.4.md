# Compact viewer actions, v0.4

## Reference review before implementation

Viewed the provided GuoheView reference image again on 2026-10-09. Its relevant qualities are a large quiet image canvas, a thin white command strip, small familiar outline glyphs with generous hit areas, and no persistent action captions inside ordinary command buttons. The artwork and app branding are not copied.

## Intended change

Keep the existing eleven-command bottom toolbar and its 34-point targets (28 at narrow widths). Extend its icon-only treatment to the image-list locate/refresh actions, inspector close, rotation, flip, crop selection, crop apply, resize apply, undo, restore-original, provenance recheck, sticky crop confirmation, and the busy-operation cancel control.

Introduce a compact vector action widget with a 32 × 32 logical-point target, shrinking only to 28 when the available width requires it. Its idle appearance is unframed. Hover, selected, disabled, pressed, and keyboard-focus states remain legible. All controls retain accessible action names. Both hover and keyboard focus reveal the same descriptive tooltip, including existing shortcuts and interaction guidance where relevant. Menu choices and the path/export dialogs retain explicit descriptive labels because choosing an operation or destination requires clarity.

## Glyph vocabulary

- Locate: a small crosshair/target, distinct from fit-to-window.
- Rotate left/right: a circular directional arrow with a clear tangent arrowhead.
- Undo: a left-returning hooked arrow, distinct from rotation.
- Refresh directory: two opposing circular arrows.
- Restore original: an image frame with a return arrow, distinct from refresh and undo.
- Recheck provenance: a shield with a check, distinct from directory refresh.
- Select crop: conventional intersecting crop corners, selected-state background while active.
- Apply crop: smaller crop corners plus a check badge.
- Apply resize: frame/diagonal size arrow plus a check badge.
- Close/cancel: a conventional ×, with task-specific accessible name and tooltip.

## Behavior and verification

No handler, enablement condition, export rule, provenance policy, undo semantics, modal isolation, or sticky crop confirmation is intentionally changed. Retain field labels and safety/status text. Verify compact target geometry and emitted paint/accessibility data with real egui frames, pointer and keyboard activation, disabled controls, hover/focus tooltips, existing keyboard/modal/crop regressions, and offscreen rendering of the actual App::ui at normal and narrow/high-DPI sizes. Record precise checks and final source hashes after implementation. Cloud egui rasterizations are not native Windows or GPU validation; all Rust UI changes require rebuilding and retesting the native binaries before release.

## Implemented controls and behavior

Converted fourteen control placements to compact icon-only commands:

1. Image list: locate current image, refresh directory.
2. Inspector: close panel, rotate clockwise, rotate counterclockwise, flip horizontally, toggle drag-crop selection, apply numeric crop, apply resize, undo, restore original, recheck provenance.
3. Sticky crop footer: apply crop.
4. Busy-operation overlay: cancel current operation.

The eleven bottom-toolbar placements were already icon-only and remain in the same order and hit-target geometry. Their rotate and undo glyphs were refined to remain distinguishable. More-menu choices, background choices, path-dialog Open, export-dialog destination chooser and final Export retain descriptive text. Field labels, editable numeric values, status text, selection dimensions, provenance warnings and safety statements remain visible.

Compact command names use stable scoped IDs. Keyboard-focused compact controls keep their advertised R, Shift+R and Ctrl+Z shortcuts, plus F10; text-entry and modal guards still apply. Inspector and image-list controls now explicitly disable behind path/export/error dialogs, matching the existing toolbar guard. Existing edit/crop/undo/export handlers are unchanged. Crop selection remains explicit and its apply control remains pinned above the toolbar.

## Cloud verification, 2026-10-09

- Passed cargo fmt --all --check.
- Passed strict workspace/all-targets Clippy for x86_64-pc-windows-msvc with GLOAMERE_WINDOWS_RESOURCE_CHECK_ONLY=1.
- Passed workspace/all-targets Windows-target cargo check with the same explicit source-check flag.
- Passed 53 product UI tests, compiled from the live product modules by the existing temporary ui-render harness, single-threaded. Seven additions verify icon-only paint and 28–32-point geometry; accessible names and disabled states; hover and keyboard-focus tooltips; pointer/Enter and selected states; distinct vector glyphs; actual inspector pointer workflows through rotate/undo/restore/resize/sticky crop/close; focused editor shortcuts; and blocked background editor/list actions in path/export/error dialogs. Existing More focus trapping, repeated toggles, text-entry isolation, Escape layering, source integrity and minimum-window sticky crop tests also pass.
- Rendered 17 views from actual App::ui output and egui tessellation with the temporary CPU renderer. Reviewed normal editor, crop, scrolled editor, list, provenance, busy cancel, keyboard-focus tooltip, and 400-logical-pixel-wide / 2× editor, sticky crop and list views. The high-DPI editor is scrolled to its command controls. Renders and render-manifest.json are temporary external QA evidence, not added to the source or uploaded.

This is cloud source, synthetic-input and CPU-render evidence only. The resource-check flag intentionally does not generate Windows resources or a runnable native EXE. Rebuild both Windows native binaries and rerun native interaction, resource and installation QA against these new Rust sources before release; prior native binaries cannot validate this change.

Final UI source SHA-256:

- src/app.rs: 34a3ae871a1670e992d3dd84c8e8bb1f1303920acf0ae3c3b7c995ddc790686b
- src/viewer_ui.rs: cfeb79b985885a644173a56602bca7179964356c3b4e9c556a26e19623334fff
- src/app_keyboard_tests.rs: 7489b047c3e84730f0f572de34eaaa472a1a0eef38347e8af2c04e3053c8f570
