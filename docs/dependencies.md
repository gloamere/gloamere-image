# Dependency review

2026-10-07 registry checked: Rust stable 1.99.0, eframe 0.36.2, image 0.25.10,
moxcms latest 0.9.1, official contentauth c2pa 0.91.2. Implementation locks moxcms
0.8.1 to reuse image 0.25.10's dependency; eframe uses wgpu 30.0.1. Rust MSRV 1.96.
Lock Cargo.lock before delivery.
eframe uses wgpu only; Windows explicitly enables dx12, Mac later metal.
image enables only JPEG/PNG, uses upstream decoders and limits, no custom decoder.
Color transforms use moxcms ICC parsing and sRGB destination; malformed/unsupported
profiles must report unverified color instead of silently claiming correctness.
rfd is only platform dialog boundary; portable app core and UI remain shared.
C2PA will be a separate worker with minimal features; do not enable default HTTP.
Sources: https://docs.rs/eframe/0.36.2/eframe/ ; https://docs.rs/image/0.25.10/image/ ;
https://docs.rs/moxcms/0.8.1/moxcms/ ; https://github.com/contentauth/c2pa-rs .
