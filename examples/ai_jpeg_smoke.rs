//! Explicit local-only JPEG decoder QA; never discovers a pack or user images.
//! Usage: ai_jpeg_smoke <verified-pack-directory> <19-fixture-directory> <report-json>
//! This command uses only the pinned synthetic/CC0 engineering corpus below.
//! It is not a product test dependency, model-quality evaluation, or worker
//! isolation test. Run it explicitly for each final native platform component.
use light_image_tool::{
    ToolError,
    ai::{jpeg, model},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
};

type QaResult<T> = Result<T, Box<dyn std::error::Error>>;
const MAX_JPEG: u64 = 64 * 1024 * 1024;

struct Fixture {
    name: &'static str,
    width: u32,
    height: u32,
    jpeg_sha256: &'static str,
    rgb_sha256: &'static str,
}
// Independent Pillow 12.3.0 / libjpeg-turbo 3.1.4.1 reference bytes, pinned
// before this adapter was integrated. No input or reference is regenerated.
const FIXTURES: [Fixture; 19] = [
    Fixture {
        name: "odd-ramp-227x229-s0.jpg",
        width: 227,
        height: 229,
        jpeg_sha256: "654005cca57e9a5e10f0083ef26dd99cb5fd9ef099138b32500fd234ef57fd49",
        rgb_sha256: "08cde6c6c8a51b20a3feb201704bf9faaa0f45024887bc1dc072fbf32f6d7a99",
    },
    Fixture {
        name: "odd-progressive-227x229-s0.jpg",
        width: 227,
        height: 229,
        jpeg_sha256: "de78fd7c5e85a0834439ca1341964242f8f57fbed9a6774102045f8684373ab8",
        rgb_sha256: "b65ce00a568c427bae2e41e0c4a409131f67d51329680c903ad5b2ac15e084f1",
    },
    Fixture {
        name: "odd-ramp-227x229-s1.jpg",
        width: 227,
        height: 229,
        jpeg_sha256: "921613349b14b476fe4e0eef28cef039252be0e6906e90b9333682489f290864",
        rgb_sha256: "d426952e134bdb1337d935f3af33f49e77c473d77f4d60ba6ca4458435e937a7",
    },
    Fixture {
        name: "odd-progressive-227x229-s1.jpg",
        width: 227,
        height: 229,
        jpeg_sha256: "10d6540c40f71b658ed8d2875ca69baebf695dd866c521bec136e714879d4671",
        rgb_sha256: "3a79cc06520ff6d1fc7c532a56e7c8536c6162eacf5056650701dce5a4db7bc1",
    },
    Fixture {
        name: "odd-ramp-227x229-s2.jpg",
        width: 227,
        height: 229,
        jpeg_sha256: "9eb236c80749ee10331724b226dac3a47776fde7054a20ae38d498b5fddd5ace",
        rgb_sha256: "271a0f80977202caa6d4cccfb729244c6618979e610fcdb464c6f0c5f858d778",
    },
    Fixture {
        name: "odd-progressive-227x229-s2.jpg",
        width: 227,
        height: 229,
        jpeg_sha256: "2ca1a1e6833c9f6a6c9b53793d6ce48f65176dcc178b164ad39c7d56cbdb46d1",
        rgb_sha256: "7edc2e2d4da1b8fe5fd7167d9053b993de7e0d89eed14190dda3a4637e2ec7b4",
    },
    Fixture {
        name: "noise-17x19-s0.jpg",
        width: 17,
        height: 19,
        jpeg_sha256: "0d96db191afdc7c20ff69b2b00f785d7e0a91c004dbace6cb761e30d5de3d5c0",
        rgb_sha256: "b19626e6edbc2b8b039412a8579817fb2ec85ebea1681b0bfe8caf51410c71c0",
    },
    Fixture {
        name: "noise-17x19-s1.jpg",
        width: 17,
        height: 19,
        jpeg_sha256: "9b6ed2e953f513367bb6474ca51ec2432011c04dbe600704acf14acb4ac38f99",
        rgb_sha256: "475665907c59c9ee191a832151a464edca8127e1ae4ef8c167d130c27fe4b411",
    },
    Fixture {
        name: "noise-17x19-s2.jpg",
        width: 17,
        height: 19,
        jpeg_sha256: "b3c0760db017db337d6bc8191e89251ad01d5b6f68ca1abf464abf86681cb592",
        rgb_sha256: "43c0acc7f8eedc7c88896b00467817c7792dac00ce281e5e4f31ab590631ba2f",
    },
    Fixture {
        name: "gray-227x229-p0.jpg",
        width: 227,
        height: 229,
        jpeg_sha256: "ce28713b72f34b2f2770a3215cccc8fe3e162ffa50c8356357215535b42be397",
        rgb_sha256: "307a63bd03eee9e3a4743e85db51c208e919ddb5c5ddc278a3476a9cab7ce57d",
    },
    Fixture {
        name: "gray-227x229-p1.jpg",
        width: 227,
        height: 229,
        jpeg_sha256: "8b57d88b3e2f1c3013abab43605c738a9dd21d8f7e5232350b41a6ca9101442f",
        rgb_sha256: "307a63bd03eee9e3a4743e85db51c208e919ddb5c5ddc278a3476a9cab7ce57d",
    },
    Fixture {
        name: "tiny-1x1.jpg",
        width: 1,
        height: 1,
        jpeg_sha256: "ba542725805e7fff248ad2c3ac9bbe1d07fa8f231649d7b3186ddef45f68fbec",
        rgb_sha256: "709e80c88487a2411e1ee4dfb9f22a861492d20c4765150c0c794abd70f8147c",
    },
    Fixture {
        name: "tiny-1x17.jpg",
        width: 1,
        height: 17,
        jpeg_sha256: "6f707d64c6b302e8c05a5f86ffe24acaf1cf5b68dee1dbd052480a672c7f1a6f",
        rgb_sha256: "ce2630d1588c7dc7c16514e0bdfe3db4db2889d8eae902ac51cca9971c19edab",
    },
    Fixture {
        name: "tiny-17x1.jpg",
        width: 17,
        height: 1,
        jpeg_sha256: "9ee6eff08c2b466944d4f74d77a50b34ed1954618bbf55c65379cd2e597cf8e1",
        rgb_sha256: "e96655d93dd7e09a527cbb46e1f1f0ccb5ea2d14632aa68c5c2d52da9fa7c7a2",
    },
    Fixture {
        name: "tiny-9x7.jpg",
        width: 9,
        height: 7,
        jpeg_sha256: "9eb00ab9dc653ab953fa30cf4ecd8407328adaf65a4d92735fba827549de1f96",
        rgb_sha256: "ece98351bdcb2321e73a8f17a80286ab23cb63c9085d445f5cf84ffa4ccb7571",
    },
    Fixture {
        name: "coffee-599x399-s0.jpg",
        width: 599,
        height: 399,
        jpeg_sha256: "f3f8a405b5fc6728308a03ebe28007b56c73b5f86ccdbe741bed0a9bcae57aae",
        rgb_sha256: "46412a9410141df66035c232b6f8805b08c301146fbdec8492ce0c39cdf390c2",
    },
    Fixture {
        name: "coffee-599x399-s1.jpg",
        width: 599,
        height: 399,
        jpeg_sha256: "39d7f169cf4cd364aea2877518f105d42a089d173a246144c169720257376786",
        rgb_sha256: "fcc9cd2c97ef80fac566eb63c9953c31ff0661efca656eedd33f1503367f528a",
    },
    Fixture {
        name: "coffee-599x399-s2.jpg",
        width: 599,
        height: 399,
        jpeg_sha256: "17ecdec250c7bdf6ecccda88b3dcd5aaff6fe8480dd63ca78a58cf21f8b445a1",
        rgb_sha256: "a38f39f302fdcdfce7505cbbc24ee0737ee324a8801f80fb24bcb1e293136cf9",
    },
    Fixture {
        name: "original-codec-ramp.jpg",
        width: 640,
        height: 480,
        jpeg_sha256: "e54fdae788eb0de47ff1d448c57963816ce6a24332f7898622c57e1db80b9c8b",
        rgb_sha256: "8dc327c42a2e1c277d581ffe53ed0603d7ba9efb0a766da40523372e611e796b",
    },
];

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn read_file(path: &Path, max: u64) -> QaResult<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > max {
        return Err(format!("not a bounded regular file: {}", path.display()).into());
    }
    let mut bytes = Vec::new();
    File::open(path)?.take(max + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err(format!("file grew beyond limit: {}", path.display()).into());
    }
    Ok(bytes)
}

fn fixture_result(fixture: &Fixture, directory: &Path, library: &Path) -> QaResult<Value> {
    let path = directory.join(fixture.name);
    let bytes = read_file(&path, MAX_JPEG)?;
    let encoded_hash = sha256(&bytes);
    if encoded_hash != fixture.jpeg_sha256 {
        return Err(format!("pinned JPEG hash mismatch: {}", fixture.name).into());
    }
    let channels = u64::from(fixture.width) * u64::from(fixture.height) * 3;
    let reference = read_file(&path.with_extension("pillow.rgb8"), channels)?;
    let reference_hash = sha256(&reference);
    if reference.len() as u64 != channels || reference_hash != fixture.rgb_sha256 {
        return Err(format!("pinned Pillow RGB hash/length mismatch: {}", fixture.name).into());
    }
    let image = jpeg::decode_rgb(&bytes, library)?;
    let actual = image.as_raw();
    let different_channels = actual
        .iter()
        .zip(&reference)
        .filter(|(a, b)| a != b)
        .count()
        + actual.len().abs_diff(reference.len());
    let max_abs_error = actual
        .iter()
        .zip(&reference)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(0);
    let sum_abs_error: u64 = actual
        .iter()
        .zip(&reference)
        .map(|(a, b)| u64::from(a.abs_diff(*b)))
        .sum();
    let passed = image.dimensions() == (fixture.width, fixture.height)
        && actual.len() == reference.len()
        && different_channels == 0;
    Ok(json!({
        "fixture":fixture.name, "jpeg_sha256":encoded_hash,
        "width":image.width(), "height":image.height(),
        "expected_width":fixture.width, "expected_height":fixture.height,
        "rgb_sha256":sha256(actual), "reference_rgb_sha256":reference_hash,
        "channels":actual.len(), "reference_channels":reference.len(),
        "different_channels":different_channels, "max_abs_error":max_abs_error,
        "sum_abs_error":sum_abs_error,
        "mean_abs_error":sum_abs_error as f64 / reference.len() as f64,
        "passed":passed
    }))
}

fn sof(marker: u8, precision: u8, components: u8) -> Vec<u8> {
    let mut bytes = vec![0xff, 0xd8, 0xff, marker];
    bytes.extend_from_slice(&(8 + u16::from(components) * 3).to_be_bytes());
    bytes.extend_from_slice(&[precision, 0, 227, 0, 229, components]);
    for component in 1..=components {
        bytes.extend_from_slice(&[component, 0x11, 0]);
    }
    bytes.extend_from_slice(&[0xff, 0xda, 0, 2]);
    bytes
}

fn rejection_result(name: &str, bytes: &[u8], unsupported: bool, library: &Path) -> Value {
    let (kind, message) = match jpeg::decode_rgb(bytes, library) {
        Err(ToolError::Invalid(message)) if message.starts_with("不支持：") => {
            ("unsupported", message)
        }
        Err(ToolError::Invalid(message)) => ("malformed_or_limit", message),
        Err(error) => ("unexpected_error_type", error.to_string()),
        Ok(_) => ("unexpected_success", String::new()),
    };
    let expected = if unsupported {
        "unsupported"
    } else {
        "malformed_or_limit"
    };
    json!({"case":name, "input_sha256":sha256(bytes), "bytes":bytes.len(),
        "expected_kind":expected, "actual_kind":kind, "error":message,
        "passed":kind == expected})
}

fn negative_results(directory: &Path, library: &Path) -> QaResult<Vec<Value>> {
    // This fixture has already passed its immutable hash check in the suite.
    let fixture = FIXTURES
        .iter()
        .find(|f| f.name == "odd-ramp-227x229-s0.jpg")
        .ok_or("missing baseline fixture definition")?;
    let bytes = read_file(&directory.join(fixture.name), MAX_JPEG)?;
    if sha256(&bytes) != fixture.jpeg_sha256 {
        return Err("negative-test baseline changed after verification".into());
    }
    let mut results = Vec::new();
    for length in [2, 10, bytes.len() / 2, bytes.len() - 2] {
        results.push(rejection_result(
            &format!("truncated_at_{length}"),
            &bytes[..length],
            false,
            library,
        ));
    }
    for (name, input, unsupported) in [
        ("missing_soi", vec![0; 16], false),
        (
            "zero_segment_length",
            vec![0xff, 0xd8, 0xff, 0xe0, 0, 0],
            false,
        ),
        ("missing_scan_tables", sof(0xc0, 8, 3), false),
        ("lossless_sof3", sof(0xc3, 8, 3), true),
        ("arithmetic_sof9", sof(0xc9, 8, 3), true),
        ("higher_precision", sof(0xc0, 12, 3), true),
        ("four_components", sof(0xc0, 8, 4), true),
    ] {
        results.push(rejection_result(name, &input, unsupported, library));
    }
    // Exercise the native-header colorspace gate using a pinned 4:4:4 JPEG
    // whose three component IDs are changed to R/G/B. Scan bytes are unchanged;
    // the input is rejected before pixel decode. This is not an RGB golden.
    let offset = bytes
        .windows(2)
        .position(|pair| pair == [0xff, 0xc0])
        .ok_or("baseline fixture lost SOF0")?;
    let mut direct_rgb = bytes.clone();
    for (index, id) in (*b"RGB").into_iter().enumerate() {
        direct_rgb[offset + 10 + index * 3] = id;
    }
    // Remove the JFIF APP0 marker, whose colorspace declaration takes precedence
    // over component IDs in libjpeg's header inference.
    if direct_rgb.get(2..4) == Some(&[0xff, 0xe0]) {
        let length = usize::from(u16::from_be_bytes([direct_rgb[4], direct_rgb[5]]));
        direct_rgb.drain(2..4 + length);
    }
    let scan = direct_rgb
        .windows(2)
        .position(|pair| pair == [0xff, 0xda])
        .ok_or("baseline fixture lost SOS")?;
    for (index, id) in (*b"RGB").into_iter().enumerate() {
        direct_rgb[scan + 5 + index * 2] = id;
    }
    results.push(rejection_result(
        "native_direct_rgb_header",
        &direct_rgb,
        true,
        library,
    ));
    let mut sampling_440 = bytes;
    sampling_440[offset + 11] = 0x12;
    results.push(rejection_result(
        "native_unvalidated_440_sampling",
        &sampling_440,
        true,
        library,
    ));
    Ok(results)
}

fn main() -> QaResult<()> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.len() != 3 {
        return Err(
            "usage: ai_jpeg_smoke <verified-pack-directory> <19-fixture-directory> <report-json>"
                .into(),
        );
    }
    let pack = model::validate_pack(&PathBuf::from(&arguments[0]))?;
    let fixtures = PathBuf::from(&arguments[1]).canonicalize()?;
    let output = PathBuf::from(&arguments[2]);
    let library = pack.directory.join(&pack.manifest.jpeg_decoder.name);
    // Keep a checked file handle alive across every native call. On Windows,
    // deny replacement/writes while the verified component is mapped. On Unix,
    // this explicit local QA assumes the pack is not concurrently modified.
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ);
        light_image_tool::ai::windows::harden_dll_search()?;
    }
    let mut component_guard = options.open(&library)?;
    let mut component_bytes = Vec::new();
    (&mut component_guard)
        .take(pack.manifest.jpeg_decoder.bytes + 1)
        .read_to_end(&mut component_bytes)?;
    if component_bytes.len() as u64 != pack.manifest.jpeg_decoder.bytes
        || sha256(&component_bytes) != pack.manifest.jpeg_decoder.sha256
    {
        return Err("JPEG component changed after pack validation".into());
    }
    let mut vectors = Vec::new();
    for fixture in &FIXTURES {
        vectors.push(match fixture_result(fixture, &fixtures, &library) {
            Ok(record) => record,
            Err(error) => {
                json!({"fixture":fixture.name, "passed":false, "error":error.to_string()})
            }
        });
    }
    let negatives = match negative_results(&fixtures, &library) {
        Ok(records) => records,
        Err(error) => vec![
            json!({"case":"negative_fixture_integrity", "passed":false, "error":error.to_string()}),
        ],
    };
    let passed = vectors
        .iter()
        .chain(&negatives)
        .all(|r| r["passed"] == true);
    let total_channels: u64 = vectors
        .iter()
        .filter_map(|r| r["reference_channels"].as_u64())
        .sum();
    let report = json!({
        "schema_version":1,
        "scope":"Pinned 19-fixture decoder parity and rejection behavior only; no classifier quality or worker-isolation claim",
        "platform":std::env::consts::OS, "architecture":std::env::consts::ARCH,
        "pack_schema_version":pack.manifest.schema_version,
        "preprocess_version":pack.manifest.preprocess_version,
        "decoder_component":pack.manifest.jpeg_decoder,
        "reference":"Pillow 12.3.0 / libjpeg-turbo 3.1.4.1; Image.open(...).convert('RGB'); no resize/EXIF transpose/ICC transform",
        "vectors":vectors, "negative_cases":negatives,
        "expected_fixture_count":19, "total_reference_channels":total_channels,
        "all_passed":passed, "quality_approval":false
    });
    fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
    drop(component_guard);
    println!("{}", output.display());
    if !passed {
        return Err("JPEG decoder QA failed; inspect the written report".into());
    }
    Ok(())
}
