# Synthetic lossy WebP fixtures

Generated offline on 2026-10-09 with Pillow 12.3.0 / libwebp 1.6.0. These 2×2 plain-color fixtures contain no private or third-party image content.

- `lossy-red.webp`: RGB `(255, 0, 0)`, quality 100, method 6, VP8.
- `lossy-alpha-red.webp`: RGBA `(255, 0, 0, 128)`, quality 100, method 6, ALPH + VP8.

Independent Pillow/libwebp decode gives every pixel `(255, 1, 0, 255)` and `(255, 1, 0, 128)` respectively. Tests assert those exact values through both the static product pipeline and the animation's virtual standalone-frame reader. GIF and lossless WebP fixtures are generated deterministically inside `tests/animation.rs` from explicit indexed colors/frame rectangles; no runtime Python dependency.
