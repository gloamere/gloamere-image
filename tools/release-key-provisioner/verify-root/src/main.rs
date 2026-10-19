//! Test-only bridge. Never ship or enable this verifier in a release build.
use safeupdate_core::{CancellationToken, LockedState, ProductionConfig};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    test_only: bool,
    root: String,
    public_bundle: String,
    metadata: BTreeMap<String, String>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("fixture path required")?;
    let bytes = std::fs::read(path)?;
    if bytes.len() > 1024 * 1024 {
        return Err("oversized ephemeral fixture".into());
    }
    let fixture: Fixture = serde_json::from_slice(&bytes)?;
    if !fixture.test_only {
        return Err("only ephemeral test fixtures accepted".into());
    }
    let config = ProductionConfig::from_reviewed_public_bundle(
        fixture.public_bundle.as_bytes(),
        fixture.root.as_bytes(),
    )?;
    let scratch = tempfile::tempdir()?;
    let mut state = LockedState::initialize(&scratch.path().join("state"), &config)?;
    safeupdate_core::fixture::verify_fixture_repository(
        &config,
        &mut state,
        fixture
            .metadata
            .into_iter()
            .map(|(name, bytes)| (name, bytes.into_bytes()))
            .collect(),
        &CancellationToken::new(),
    )?;
    println!("Ephemeral Python root and signed metadata accepted by tough and the app profile.");
    Ok(())
}
