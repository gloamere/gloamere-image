use image::{ImageEncoder, RgbaImage};
use light_image_tool::{
    Cancellation, ToolError,
    animation::{self, AnimationRequest, AnimationWorker, LoopPolicy, PlaybackClock},
    pipeline,
};
use std::{
    borrow::Cow,
    fs,
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Clone)]
struct GifSpec {
    left: u16,
    top: u16,
    width: u16,
    height: u16,
    pixels: Vec<u8>,
    delay: u16,
    disposal: gif::DisposalMethod,
}
fn gif_bytes(width: u16, height: u16, repeat: Option<gif::Repeat>, specs: &[GifSpec]) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = gif::Encoder::new(
            &mut bytes,
            width,
            height,
            &[0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255],
        )
        .unwrap();
        if let Some(repeat) = repeat {
            encoder.set_repeat(repeat).unwrap();
        }
        for spec in specs {
            encoder
                .write_frame(&gif::Frame {
                    width: spec.width,
                    height: spec.height,
                    left: spec.left,
                    top: spec.top,
                    delay: spec.delay,
                    dispose: spec.disposal,
                    transparent: Some(0),
                    buffer: Cow::Borrowed(&spec.pixels),
                    ..Default::default()
                })
                .unwrap();
        }
    }
    bytes
}
fn gif_spec(left: u16, width: u16, pixels: Vec<u8>, disposal: gif::DisposalMethod) -> GifSpec {
    GifSpec {
        left,
        top: 0,
        width,
        height: 1,
        pixels,
        delay: 3,
        disposal,
    }
}
fn load(bytes: &[u8]) -> pipeline::LoadedImage {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("内容检测.dat");
    fs::write(&path, bytes).unwrap();
    pipeline::load(&path, &Cancellation::default()).unwrap()
}
fn load_source(bytes: &[u8]) -> Arc<animation::AnimationSource> {
    load(bytes).animation.unwrap()
}
fn colors(image: &RgbaImage) -> Vec<[u8; 4]> {
    image.pixels().map(|p| p.0).collect()
}
const R: [u8; 4] = [255, 0, 0, 255];
const G: [u8; 4] = [0, 255, 0, 255];
const B: [u8; 4] = [0, 0, 255, 255];
const T: [u8; 4] = [0, 0, 0, 0];
fn chunk(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = kind.to_vec();
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    if !payload.len().is_multiple_of(2) {
        out.push(0);
    }
    out
}
fn riff(chunks: &[u8]) -> Vec<u8> {
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&((chunks.len() + 4) as u32).to_le_bytes());
    out.extend_from_slice(b"WEBP");
    out.extend_from_slice(chunks);
    out
}
fn u24(n: u32) -> [u8; 3] {
    n.to_le_bytes()[..3].try_into().unwrap()
}
#[derive(Clone)]
struct WebpSpec {
    x: u32,
    y: u32,
    pixels: RgbaImage,
    delay: u32,
    flags: u8,
}
fn webp_spec(x: u32, pixels: &[[u8; 4]], delay: u32, flags: u8) -> WebpSpec {
    WebpSpec {
        x,
        y: 0,
        pixels: RgbaImage::from_raw(
            pixels.len() as u32,
            1,
            pixels.iter().flatten().copied().collect(),
        )
        .unwrap(),
        delay,
        flags,
    }
}
fn animated_webp(
    width: u32,
    height: u32,
    loops: u16,
    specs: &[WebpSpec],
    metadata: Option<(&[u8; 4], &[u8])>,
) -> Vec<u8> {
    let mut header = vec![0x12, 0, 0, 0];
    if let Some((kind, _)) = metadata {
        if kind == b"ICCP" {
            header[0] |= 0x20;
        } else if kind == b"EXIF" {
            header[0] |= 0x08;
        }
    }
    header.extend_from_slice(&u24(width - 1));
    header.extend_from_slice(&u24(height - 1));
    let mut chunks = chunk(b"VP8X", &header);
    if let Some((kind, data)) = metadata
        && kind == b"ICCP"
    {
        chunks.extend(chunk(kind, data));
    }
    let mut anim = vec![0, 0, 0, 0];
    anim.extend_from_slice(&loops.to_le_bytes());
    chunks.extend(chunk(b"ANIM", &anim));
    for spec in specs {
        let mut encoded = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut encoded)
            .write_image(
                spec.pixels.as_raw(),
                spec.pixels.width(),
                spec.pixels.height(),
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        let mut payload = Vec::new();
        payload.extend_from_slice(&u24(spec.x / 2));
        payload.extend_from_slice(&u24(spec.y / 2));
        payload.extend_from_slice(&u24(spec.pixels.width() - 1));
        payload.extend_from_slice(&u24(spec.pixels.height() - 1));
        payload.extend_from_slice(&u24(spec.delay));
        payload.push(spec.flags);
        payload.extend_from_slice(&encoded[12..]);
        chunks.extend(chunk(b"ANMF", &payload));
    }
    if let Some((kind, data)) = metadata
        && kind != b"ICCP"
    {
        chunks.extend(chunk(kind, data));
    }
    riff(&chunks)
}
fn tiff(orientation: u8) -> Vec<u8> {
    vec![
        b'I',
        b'I',
        42,
        0,
        8,
        0,
        0,
        0,
        1,
        0,
        0x12,
        1,
        3,
        0,
        1,
        0,
        0,
        0,
        orientation,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ]
}

#[test]
fn gif_disposal_transparency_and_delay_pixel_goldens() {
    use gif::DisposalMethod::*;
    let mut specs = vec![
        gif_spec(0, 3, vec![1, 1, 1], Keep),
        gif_spec(1, 1, vec![2], Previous),
        gif_spec(2, 1, vec![3], Background),
        gif_spec(0, 1, vec![2], Keep),
        gif_spec(1, 1, vec![0], Keep),
    ];
    specs[0].delay = 0;
    let source = load_source(&gif_bytes(3, 1, None, &specs));
    assert_eq!(source.loops, LoopPolicy::Finite(1));
    assert_eq!(source.first_delay, Duration::from_millis(20));
    let mut stream = source.open(&Cancellation::default()).unwrap();
    for (index, expected) in [
        vec![R, R, R],
        vec![R, G, R],
        vec![R, R, B],
        vec![G, R, T],
        vec![G, R, T],
    ]
    .into_iter()
    .enumerate()
    {
        let frame = stream.next_frame().unwrap().unwrap();
        assert_eq!(colors(&frame.pixels), expected);
        assert_eq!(frame.frame_index, index as u32 + 1);
        assert_eq!(frame.delay_normalized, index == 0);
    }
    assert!(stream.next_frame().unwrap().is_none());
}
#[test]
fn gif_finite_repeat_counts_additional_traversals_and_infinite_is_explicit() {
    use gif::DisposalMethod::*;
    let specs = [gif_spec(0, 1, vec![1], Keep), gif_spec(1, 1, vec![2], Keep)];
    for (repeat, expected) in [
        (Some(gif::Repeat::Finite(1)), LoopPolicy::Finite(2)),
        (None, LoopPolicy::Finite(1)),
        (Some(gif::Repeat::Infinite), LoopPolicy::Infinite),
    ] {
        let source = load_source(&gif_bytes(2, 1, repeat, &specs));
        assert_eq!(source.loops, expected);
        let mut stream = source.open(&Cancellation::default()).unwrap();
        let count = match expected {
            LoopPolicy::Finite(n) => n * 2,
            LoopPolicy::Infinite => 10,
        };
        for n in 0..count {
            let frame = stream.next_frame().unwrap().unwrap();
            assert_eq!(frame.traversal, n / 2 + 1);
            assert_eq!(
                colors(&frame.pixels),
                if n % 2 == 0 { vec![R, T] } else { vec![R, G] }
            );
        }
        if matches!(expected, LoopPolicy::Finite(_)) {
            assert!(stream.next_frame().unwrap().is_none());
        }
    }
}
#[test]
fn single_frame_gif_and_static_lossless_webp_keep_static_edit_path() {
    let gif = load(&gif_bytes(
        1,
        1,
        None,
        &[gif_spec(0, 1, vec![1], gif::DisposalMethod::Keep)],
    ));
    assert!(gif.animation.is_none());
    assert_eq!(colors(&gif.pixels), vec![R]);
    let input = RgbaImage::from_raw(2, 1, [R, [90, 40, 20, 128]].concat()).unwrap();
    let mut bytes = Vec::new();
    image::codecs::webp::WebPEncoder::new_lossless(&mut bytes)
        .write_image(input.as_raw(), 2, 1, image::ExtendedColorType::Rgba8)
        .unwrap();
    let loaded = load(&bytes);
    assert!(loaded.animation.is_none());
    assert_eq!(*loaded.pixels, input);
    assert!(loaded.has_alpha);
}
#[test]
fn webp_blend_replace_background_disposal_and_loop_reset_goldens() {
    let specs = [
        webp_spec(0, &[R, R, R], 40, 2),
        webp_spec(0, &[[0, 0, 255, 128]], 30, 0),
        webp_spec(2, &[G], 50, 1),
        webp_spec(0, &[T], 60, 2),
    ];
    let source = load_source(&animated_webp(3, 1, 1, &specs, None));
    assert_eq!(source.loops, LoopPolicy::Finite(1));
    let mut stream = source.open(&Cancellation::default()).unwrap();
    for expected in [
        vec![R, R, R],
        vec![[127, 0, 128, 255], R, R],
        vec![[127, 0, 128, 255], R, G],
        vec![T, R, T],
    ] {
        let frame = stream.next_frame().unwrap().unwrap();
        assert_eq!(colors(&frame.pixels), expected);
    }
    assert!(stream.next_frame().unwrap().is_none());
    let source = load_source(&animated_webp(
        3,
        1,
        2,
        &[webp_spec(0, &[R], 30, 2), webp_spec(2, &[G], 30, 2)],
        None,
    ));
    let mut stream = source.open(&Cancellation::default()).unwrap();
    for expected in [vec![R, T, T], vec![R, T, G], vec![R, T, T], vec![R, T, G]] {
        assert_eq!(
            colors(&stream.next_frame().unwrap().unwrap().pixels),
            expected
        );
    }
    assert!(stream.next_frame().unwrap().is_none());
}
#[test]
fn webp_orientation_is_applied_once_to_composited_canvas_and_icc_failure_is_explicit() {
    let specs = [webp_spec(0, &[R, G, B], 0, 2), webp_spec(2, &[T], 1, 2)];
    let bytes = animated_webp(3, 1, 1, &specs, Some((b"EXIF", &tiff(6))));
    let loaded = load(&bytes);
    assert_eq!(loaded.orientation, 6);
    assert_eq!(loaded.pixels.dimensions(), (1, 3));
    assert_eq!(colors(&loaded.pixels), vec![R, G, B]);
    let source = loaded.animation.unwrap();
    drop(loaded.pixels);
    let mut stream = source.open(&Cancellation::default()).unwrap();
    drop(stream.next_frame().unwrap());
    let next = stream.next_frame().unwrap().unwrap();
    assert_eq!(next.pixels.dimensions(), (1, 3));
    assert_eq!(colors(&next.pixels), vec![R, G, T]);
    assert!(next.delay_normalized);
    let bad = load(&animated_webp(3, 1, 1, &specs, Some((b"ICCP", b"bad ICC"))));
    assert!(!bad.color_managed);
    assert!(bad.warnings.iter().any(|w| w.contains("ICC")));
}
#[test]
fn animated_canvas_limit_and_malformed_bitstream_dimension_guard() {
    assert!(animation::validate_animation_dimensions(2000, 2000).is_ok());
    assert!(animation::validate_animation_dimensions(2001, 2000).is_err());
    let mut vp8 = vec![0, 0, 0, 0x9d, 1, 0x2a, 0xff, 0x3f, 0xff, 0x3f];
    vp8.extend_from_slice(&[0; 8]);
    let mut extended = chunk(b"VP8X", &[0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    extended.extend(chunk(b"ANIM", &[0; 6]));
    let mut frame = vec![0; 16];
    frame[15] = 2;
    frame.extend(chunk(b"VP8 ", &vp8));
    extended.extend(chunk(b"ANMF", &frame));
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("mismatch.webp");
    fs::write(&path, riff(&extended)).unwrap();
    let error = pipeline::load(&path, &Cancellation::default())
        .err()
        .unwrap();
    assert!(error.to_string().contains("尺寸"));
    // Extended static inputs take the same guard before image's decoder path.
    let mut extended = chunk(b"VP8X", &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    extended.extend(chunk(b"VP8 ", &vp8));
    fs::write(&path, riff(&extended)).unwrap();
    assert!(pipeline::load(&path, &Cancellation::default()).is_err());
}
#[test]
fn frame_limit_accepts_2000_rejects_2001_without_losing_last_frame() {
    for count in [2000, 2001] {
        let spec = gif_spec(0, 1, vec![1], gif::DisposalMethod::Keep);
        let source = load_source(&gif_bytes(1, 1, None, &vec![spec; count]));
        let mut stream = source.open(&Cancellation::default()).unwrap();
        let mut last = None;
        for _ in 0..2000 {
            last = stream.next_frame().unwrap();
        }
        assert_eq!(colors(&last.as_ref().unwrap().pixels), vec![R]);
        let next = stream.next_frame();
        if count == 2000 {
            assert!(next.unwrap().is_none());
        } else {
            assert!(next.unwrap_err().to_string().contains("2000"));
        }
        assert_eq!(colors(&last.unwrap().pixels), vec![R]);
    }
}
#[test]
fn bounded_ownership_and_cancellation_retain_valid_pixels() {
    let source = load_source(&gif_bytes(
        1,
        1,
        Some(gif::Repeat::Infinite),
        &[
            gif_spec(0, 1, vec![1], gif::DisposalMethod::Keep),
            gif_spec(0, 1, vec![2], gif::DisposalMethod::Keep),
        ],
    ));
    let cancel = Cancellation::default();
    let mut stream = source.open(&cancel).unwrap();
    let first = stream.next_frame().unwrap().unwrap();
    let second = stream.next_frame().unwrap().unwrap();
    assert!(matches!(
        stream.next_frame(),
        Err(ToolError::ResourceBusy(_))
    ));
    assert_eq!(colors(&first.pixels), vec![R]);
    assert_eq!(colors(&second.pixels), vec![G]);
    assert!(source.buffered_bytes() <= animation::MAX_ANIMATION_BUFFER_BYTES);
    drop(first);
    drop(second);
    drop(stream);
    let mut stream = source.open(&cancel).unwrap();
    let first = stream.next_frame().unwrap().unwrap();
    cancel.cancel();
    assert!(matches!(stream.next_frame(), Err(ToolError::Cancelled)));
    assert_eq!(colors(&first.pixels), vec![R]);
}
#[test]
fn clock_accumulates_deadlines_and_rebases_after_pause() {
    let now = Instant::now();
    let mut clock = PlaybackClock::new(Duration::from_millis(40));
    assert_eq!(clock.wake_after(now), None);
    clock.resume(now);
    for n in 1..=1000 {
        let actual = now + Duration::from_millis(n * 40 + 3);
        assert!(clock.due(actual));
        assert!(clock.accept_frame(Duration::from_millis(40), actual));
        assert_eq!(clock.wake_after(actual), Some(Duration::from_millis(37)));
    }
    clock.pause();
    assert!(!clock.due(now + Duration::from_secs(3600)));
    clock.resume(now + Duration::from_secs(3600));
    assert_eq!(
        clock.wake_after(now + Duration::from_secs(3600)),
        Some(Duration::from_millis(40))
    );
    assert!(!clock.accept_frame(Duration::from_millis(40), now + Duration::from_secs(3603)));
    assert!(!clock.is_playing());
}
fn response(worker: &AnimationWorker) -> animation::AnimationResponse {
    let end = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(response) = worker.take_result() {
            return response;
        }
        assert!(Instant::now() < end, "worker response timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
}
#[test]
fn demand_worker_steps_once_restarts_and_tags_generations() {
    let source = load_source(&gif_bytes(
        1,
        1,
        None,
        &[
            gif_spec(0, 1, vec![1], gif::DisposalMethod::Keep),
            gif_spec(0, 1, vec![2], gif::DisposalMethod::Keep),
        ],
    ));
    let mut worker = AnimationWorker::without_context();
    let token = Cancellation::default();
    worker.request(AnimationRequest {
        generation: 7,
        request_id: 1,
        source: source.clone(),
        cancel: token.clone(),
        restart: false,
    });
    let next = response(&worker);
    assert_eq!((next.generation, next.request_id), (7, 1));
    let frame = next.result.unwrap().unwrap();
    assert_eq!(frame.frame_index, 2);
    drop(frame);
    worker.request(AnimationRequest {
        generation: 7,
        request_id: 2,
        source: source.clone(),
        cancel: token,
        restart: false,
    });
    assert!(response(&worker).result.unwrap().is_none());
    worker.request(AnimationRequest {
        generation: 8,
        request_id: 3,
        source,
        cancel: Cancellation::default(),
        restart: true,
    });
    let restarted = response(&worker);
    assert_eq!(restarted.generation, 8);
    assert_eq!(restarted.result.unwrap().unwrap().frame_index, 1);
    worker.stop();
    assert!(worker.take_result().is_none());
    worker.shutdown();
}

// Tiny offline synthetic fixtures generated with Pillow/libwebp, not network samples.
const LOSSY: &[u8] = include_bytes!("fixtures/animation/lossy-red.webp");
const LOSSY_ALPHA: &[u8] = include_bytes!("fixtures/animation/lossy-alpha-red.webp");
fn anmf(x: u32, y: u32, width: u32, height: u32, flags: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&u24(x / 2));
    out.extend_from_slice(&u24(y / 2));
    out.extend_from_slice(&u24(width - 1));
    out.extend_from_slice(&u24(height - 1));
    out.extend_from_slice(&u24(30));
    out.push(flags);
    out.extend_from_slice(payload);
    chunk(b"ANMF", &out)
}
fn animated_chunks(width: u32, height: u32, frames: &[Vec<u8>]) -> Vec<u8> {
    let mut header = vec![0x12, 0, 0, 0];
    header.extend_from_slice(&u24(width - 1));
    header.extend_from_slice(&u24(height - 1));
    let mut out = chunk(b"VP8X", &header);
    out.extend(chunk(b"ANIM", &[0, 0, 0, 0, 1, 0]));
    for frame in frames {
        out.extend_from_slice(frame);
    }
    riff(&out)
}
fn lossless_payload(image: &RgbaImage) -> Vec<u8> {
    let mut encoded = Vec::new();
    image::codecs::webp::WebPEncoder::new_lossless(&mut encoded)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgba8,
        )
        .unwrap();
    encoded[12..].to_vec()
}
#[test]
fn lossy_vp8_and_alph_vp8_static_and_animated_goldens() {
    for (bytes, alpha) in [(LOSSY, 255), (LOSSY_ALPHA, 128)] {
        let loaded = load(bytes);
        assert!(loaded.animation.is_none());
        assert_eq!(colors(&loaded.pixels), vec![[255, 1, 0, alpha]; 4]);
    }
    // Full green canvas, a local opaque lossy frame disposed to transparent, then a
    // full-size transparent blend: only the previous 2x2 rectangle may be cleared.
    let green = RgbaImage::from_pixel(4, 2, image::Rgba(G));
    let transparent = RgbaImage::new(4, 2);
    let bytes = animated_chunks(
        4,
        2,
        &[
            anmf(0, 0, 4, 2, 2, &lossless_payload(&green)),
            anmf(2, 0, 2, 2, 3, &LOSSY[12..]),
            anmf(0, 0, 4, 2, 0, &lossless_payload(&transparent)),
            anmf(0, 0, 2, 2, 2, &LOSSY_ALPHA[30..]),
        ],
    );
    let source = load_source(&bytes);
    let mut stream = source.open(&Cancellation::default()).unwrap();
    let expected = [
        vec![G; 8],
        vec![
            G,
            G,
            [255, 1, 0, 255],
            [255, 1, 0, 255],
            G,
            G,
            [255, 1, 0, 255],
            [255, 1, 0, 255],
        ],
        vec![G, G, T, T, G, G, T, T],
        vec![
            [255, 1, 0, 128],
            [255, 1, 0, 128],
            T,
            T,
            [255, 1, 0, 128],
            [255, 1, 0, 128],
            T,
            T,
        ],
    ];
    for expected in expected {
        assert_eq!(
            colors(&stream.next_frame().unwrap().unwrap().pixels),
            expected
        );
    }
    assert!(stream.next_frame().unwrap().is_none());
}
#[test]
fn duplicate_canvas_and_duplicate_frame_bitstreams_fail_closed() {
    let mut data = chunk(b"VP8X", &[0x12, 0, 0, 0, 1, 0, 0, 1, 0, 0]);
    data.extend(chunk(b"VP8X", &[0x12, 0, 0, 0, 3, 0, 0, 1, 0, 0]));
    data.extend(chunk(b"ANIM", &[0; 6]));
    data.extend(anmf(0, 0, 2, 2, 2, &LOSSY[12..]));
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("duplicate.webp");
    fs::write(&path, riff(&data)).unwrap();
    assert!(pipeline::load(&path, &Cancellation::default()).is_err());
    let data = animated_chunks(
        2,
        2,
        &[anmf(0, 0, 2, 2, 2, &[&LOSSY[12..], &LOSSY[12..]].concat())],
    );
    fs::write(&path, data).unwrap();
    assert!(pipeline::load(&path, &Cancellation::default()).is_err());
    // ALPH+VP8 inner dimensions are checked before the codec can allocate a bigger YUV image.
    let mut payload = LOSSY_ALPHA[30..].to_vec();
    let position = payload.windows(4).position(|w| w == b"VP8 ").unwrap();
    payload[position + 8 + 6..position + 8 + 10].copy_from_slice(&[0xff, 0x3f, 0xff, 0x3f]);
    fs::write(
        &path,
        animated_chunks(2, 2, &[anmf(0, 0, 2, 2, 2, &payload)]),
    )
    .unwrap();
    assert!(pipeline::load(&path, &Cancellation::default()).is_err());
}
#[test]
fn later_corrupt_payload_pauses_with_last_valid_frame_still_owned() {
    let mut bad = LOSSY[12..].to_vec();
    for byte in &mut bad[18..] {
        *byte = 0xff;
    }
    let bytes = animated_chunks(
        2,
        2,
        &[anmf(0, 0, 2, 2, 2, &LOSSY[12..]), anmf(0, 0, 2, 2, 2, &bad)],
    );
    let source = load_source(&bytes);
    let mut stream = source.open(&Cancellation::default()).unwrap();
    let first = stream.next_frame().unwrap().unwrap();
    assert!(stream.next_frame().is_err());
    assert_eq!(colors(&first.pixels), vec![[255, 1, 0, 255]; 4]);
    let mut gif = gif_bytes(
        2,
        1,
        None,
        &[
            gif_spec(0, 2, vec![1, 1], gif::DisposalMethod::Keep),
            gif_spec(0, 2, vec![2, 2], gif::DisposalMethod::Keep),
        ],
    );
    gif.truncate(gif.len() - 4);
    let source = load_source(&gif);
    let mut stream = source.open(&Cancellation::default()).unwrap();
    let first = stream.next_frame().unwrap().unwrap();
    assert!(stream.next_frame().is_err());
    assert_eq!(colors(&first.pixels), vec![R, R]);
}
#[test]
fn webp_rgb_icc_transforms_every_frame_and_preserves_alpha() {
    let profile = moxcms::ColorProfile::new_srgb().encode().unwrap();
    let input = [80, 120, 160, 128];
    let bytes = animated_webp(
        1,
        1,
        1,
        &[
            webp_spec(0, &[input], 20, 2),
            webp_spec(0, &[[50, 70, 90, 64]], 20, 2),
        ],
        Some((b"ICCP", &profile)),
    );
    let loaded = load(&bytes);
    assert!(loaded.color_managed);
    assert!(loaded.color_status.contains("转换"));
    assert_eq!(loaded.pixels.get_pixel(0, 0)[3], 128);
    let source = loaded.animation.unwrap();
    drop(loaded.pixels);
    let mut stream = source.open(&Cancellation::default()).unwrap();
    drop(stream.next_frame().unwrap());
    let second = stream.next_frame().unwrap().unwrap();
    assert_eq!(second.pixels.get_pixel(0, 0)[3], 64);
}
#[test]
fn gif_four_megapixel_canvas_fits_budget_and_metadata_limit_is_not_a_frame_limit() {
    let first = GifSpec {
        left: 0,
        top: 0,
        width: 2000,
        height: 2000,
        pixels: vec![1; 4_000_000],
        delay: 3,
        disposal: gif::DisposalMethod::Keep,
    };
    let source = load_source(&gif_bytes(
        2000,
        2000,
        None,
        &[first, gif_spec(0, 1, vec![2], gif::DisposalMethod::Keep)],
    ));
    let mut stream = source.open(&Cancellation::default()).unwrap();
    let first = stream.next_frame().unwrap().unwrap();
    let second = stream.next_frame().unwrap().unwrap();
    assert_eq!(first.pixels.dimensions(), (2000, 2000));
    assert_eq!(second.pixels.get_pixel(0, 0).0, G);
    assert!(source.buffered_bytes() <= animation::MAX_ANIMATION_BUFFER_BYTES);
}

#[test]
fn rapid_100_generation_switches_drop_stale_results() {
    let source = load_source(&gif_bytes(
        1,
        1,
        Some(gif::Repeat::Infinite),
        &[
            gif_spec(0, 1, vec![1], gif::DisposalMethod::Keep),
            gif_spec(0, 1, vec![2], gif::DisposalMethod::Keep),
        ],
    ));
    let mut worker = AnimationWorker::without_context();
    for generation in 1..=100 {
        worker.stop();
        worker.request(AnimationRequest {
            generation,
            request_id: generation,
            source: source.clone(),
            cancel: Cancellation::default(),
            restart: true,
        });
        if generation.is_multiple_of(10) {
            let result = response(&worker);
            assert_eq!(result.generation, generation);
            assert_eq!(result.result.unwrap().unwrap().frame_index, 1);
        }
    }
    worker.stop();
    worker.shutdown();
    assert!(worker.take_result().is_none());
    assert!(source.buffered_bytes() <= animation::MAX_ANIMATION_BUFFER_BYTES);
}

/// Core decoder/clock soak only. No GPU upload, egui scheduling or native-Windows timing claim.
/// Default 30 real-time minutes; set GLOAMERE_ANIMATION_SOAK_SECONDS for a smoke duration.
#[test]
#[ignore = "30-minute real-time GIF/WebP decoder/clock soak; run explicitly"]
fn realtime_animation_soak_30_minutes() {
    let seconds = std::env::var("GLOAMERE_ANIMATION_SOAK_SECONDS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(1800)
        .max(1);
    let gif = gif_bytes(
        64,
        64,
        Some(gif::Repeat::Infinite),
        &[
            GifSpec {
                left: 0,
                top: 0,
                width: 64,
                height: 64,
                pixels: vec![1; 4096],
                delay: 4,
                disposal: gif::DisposalMethod::Keep,
            },
            GifSpec {
                left: 0,
                top: 0,
                width: 64,
                height: 64,
                pixels: vec![2; 4096],
                delay: 6,
                disposal: gif::DisposalMethod::Previous,
            },
        ],
    );
    let webp = animated_webp(
        64,
        64,
        0,
        &[
            WebpSpec {
                x: 0,
                y: 0,
                pixels: RgbaImage::from_pixel(64, 64, image::Rgba(R)),
                delay: 40,
                flags: 2,
            },
            WebpSpec {
                x: 0,
                y: 0,
                pixels: RgbaImage::from_pixel(64, 64, image::Rgba(G)),
                delay: 60,
                flags: 3,
            },
        ],
        None,
    );
    let sources = [load_source(&gif), load_source(&webp)];
    let cancel = Cancellation::default();
    let mut streams = [
        sources[0].open(&cancel).unwrap(),
        sources[1].open(&cancel).unwrap(),
    ];
    let mut current = [
        streams[0].next_frame().unwrap().unwrap(),
        streams[1].next_frame().unwrap().unwrap(),
    ];
    let start = Instant::now();
    let stop = start + Duration::from_secs(seconds);
    let mut clocks = [
        PlaybackClock::new(current[0].delay),
        PlaybackClock::new(current[1].delay),
    ];
    for clock in &mut clocks {
        clock.resume(start);
    }
    let baseline = light_image_tool::memory::in_use();
    let mut frames = [1u64; 2];
    let mut late = [0u64; 2];
    let mut next_report = start + Duration::from_secs(60);
    println!(
        "CORE SOAK start duration_s={seconds} global_reserved={baseline} (no GPU or OS-private-memory claim)"
    );
    while Instant::now() < stop {
        let now = Instant::now();
        for index in 0..2 {
            if clocks[index].due(now) {
                let next = streams[index].next_frame().unwrap().unwrap();
                let actual = Instant::now();
                let expected = start + next.elapsed;
                if actual.saturating_duration_since(expected) > Duration::from_millis(5) {
                    late[index] += 1;
                }
                assert!(
                    clocks[index].accept_frame(next.delay, actual),
                    "decoder over two seconds late"
                );
                assert_eq!(
                    next.pixels.get_pixel(0, 0).0,
                    if next.frame_index == 1 { R } else { G }
                );
                current[index] = next;
                frames[index] += 1;
                assert!(sources[index].buffered_bytes() <= animation::MAX_ANIMATION_BUFFER_BYTES);
            }
        }
        assert!(
            light_image_tool::memory::in_use() <= baseline + 65536,
            "owned global reservations grew during soak"
        );
        if now >= next_report {
            println!(
                "CORE SOAK elapsed_s={} frames={frames:?} late_over_5ms={late:?} global_reserved={} animation_reserved={:?}",
                start.elapsed().as_secs(),
                light_image_tool::memory::in_use(),
                [sources[0].buffered_bytes(), sources[1].buffered_bytes()]
            );
            next_report += Duration::from_secs(60);
        }
        let wait = clocks
            .iter()
            .filter_map(|clock| clock.wake_after(Instant::now()))
            .min()
            .unwrap_or(Duration::from_millis(1));
        std::thread::sleep(wait.min(stop.saturating_duration_since(Instant::now())));
    }
    for (index, frame) in current.iter().enumerate() {
        let drift = start.elapsed().saturating_sub(frame.elapsed + frame.delay);
        assert!(
            drift < Duration::from_millis(100),
            "clock end drift: {drift:?}"
        );
        println!(
            "CORE SOAK codec={index} frames={} late_over_5ms={} elapsed_ms={} owned_global={} normalized_timeline_ms={}",
            frames[index],
            late[index],
            start.elapsed().as_millis(),
            light_image_tool::memory::in_use(),
            frame.elapsed.as_millis()
        );
    }
}

#[test]
fn static_gif_above_animation_cap_still_uses_static_16mp_limit() {
    let bytes = gif_bytes(
        2001,
        2000,
        None,
        &[GifSpec {
            left: 0,
            top: 0,
            width: 2001,
            height: 2000,
            pixels: vec![1; 4_002_000],
            delay: 3,
            disposal: gif::DisposalMethod::Keep,
        }],
    );
    let loaded = load(&bytes);
    assert!(loaded.animation.is_none());
    assert_eq!(loaded.pixels.dimensions(), (2001, 2000));
    assert_eq!(loaded.pixels.get_pixel(2000, 1999).0, R);
}
#[test]
fn static_webp_all_eight_exif_orientations_preserve_alpha() {
    let input = RgbaImage::from_fn(2, 3, |x, y| {
        image::Rgba([(y * 2 + x + 1) as u8, 20, 40, 128])
    });
    let payload = lossless_payload(&input);
    let expected: [&[u8]; 8] = [
        &[1, 2, 3, 4, 5, 6],
        &[2, 1, 4, 3, 6, 5],
        &[6, 5, 4, 3, 2, 1],
        &[5, 6, 3, 4, 1, 2],
        &[1, 3, 5, 2, 4, 6],
        &[5, 3, 1, 6, 4, 2],
        &[6, 4, 2, 5, 3, 1],
        &[2, 4, 6, 1, 3, 5],
    ];
    for orientation in 1..=8 {
        let mut bytes = chunk(b"VP8X", &[0x18, 0, 0, 0, 1, 0, 0, 2, 0, 0]);
        bytes.extend_from_slice(&payload);
        bytes.extend(chunk(b"EXIF", &tiff(orientation)));
        let loaded = load(&riff(&bytes));
        assert!(loaded.animation.is_none());
        assert_eq!(loaded.orientation, orientation);
        assert_eq!(
            loaded.pixels.pixels().map(|p| p[0]).collect::<Vec<_>>(),
            expected[(orientation - 1) as usize]
        );
        assert!(loaded.pixels.pixels().all(|p| p[3] == 128));
    }
}
#[test]
fn webp_2000_frame_boundary_is_bounded_and_accepts_exact_end() {
    let payload = lossless_payload(&RgbaImage::from_pixel(1, 1, image::Rgba(R)));
    let one = anmf(0, 0, 1, 1, 2, &payload);
    for count in [2000, 2001] {
        let bytes = animated_chunks(1, 1, &vec![one.clone(); count]);
        let source = load_source(&bytes);
        let mut stream = source.open(&Cancellation::default()).unwrap();
        let mut last = None;
        for _ in 0..2000 {
            last = stream.next_frame().unwrap();
        }
        let next = stream.next_frame();
        if count == 2000 {
            assert!(next.unwrap().is_none());
        } else {
            assert!(next.unwrap_err().to_string().contains("2000"));
        }
        assert_eq!(last.unwrap().pixels.get_pixel(0, 0).0, R);
    }
}
