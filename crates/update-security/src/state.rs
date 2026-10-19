use crate::config::{MAX_METADATA_VERSION, MAX_ROOT_CHAIN};
use crate::{Error, ProductionConfig, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use tough::schema::{Role, Root, Signed, Snapshot, Targets, Timestamp};

const STATE_LIMIT: u64 = 16 * 1024 * 1024;
const STATE_FILE: &str = "trusted-state-v1.json";

/// Hash the complete authenticated target independently of HashMap iteration or
/// serde_json feature unification (including preserve_order in the UI workspace).
pub(crate) fn signed_target_identity(name: &str, target: &tough::schema::Target) -> Result<String> {
    let mut bytes = Vec::new();
    let mut serializer =
        serde_json::Serializer::with_formatter(&mut bytes, olpc_cjson::CanonicalFormatter::new());
    (name, target).serialize(&mut serializer)?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MetadataFloor {
    pub version: u64,
    pub signed_sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoredMetadata {
    pub root_version: u64,
    pub envelope: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReleaseFloor {
    pub sequence: u64,
    pub identity: String,
}

/// Read-only snapshot of monotonic trust state. Installed/healthy app state lives
/// elsewhere; restoring a healthy snapshot must never restore this file.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustState {
    pub schema: u32,
    pub configuration_binding: String,
    pub last_observed_unix_seconds: i64,
    pub highest_metadata: BTreeMap<String, MetadataFloor>,
    pub(crate) root_chain: Vec<Value>,
    pub(crate) metadata: BTreeMap<String, StoredMetadata>,
    pub(crate) releases: BTreeMap<String, ReleaseFloor>,
    pub(crate) quarantined_app_sequences: BTreeSet<u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) quarantined_model_sequences: BTreeMap<String, BTreeSet<u64>>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    // Integrity checksum detects corruption, not malicious same-user edits.
    sha256: String,
    payload: TrustState,
}

#[derive(Debug)]
pub struct LockedState {
    directory: PathBuf,
    _lock: File,
    pub(crate) state: TrustState,
}

impl LockedState {
    /// Explicit first-run provisioning. Refuses an existing state or orphaned
    /// contents; callers must not convert open/read failures into initialization.
    pub fn initialize(path: &Path, config: &ProductionConfig) -> Result<Self> {
        if !path.exists() {
            let parent = path
                .parent()
                .ok_or(Error::State("state path has no parent"))?;
            check_directory(parent)?;
            fs::create_dir(path)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
            }
        }
        check_directory(path)?;
        let lock = lock_directory(path)?;
        for entry in fs::read_dir(path)? {
            if entry?.file_name() != "trust.lock" {
                return Err(Error::State("first-run directory is not empty"));
            }
        }
        let root: Signed<Root> = serde_json::from_slice(&config.pinned_root)?;
        let mut floors = BTreeMap::new();
        floors.insert("root".into(), floor(&root.signed)?);
        let state = TrustState {
            schema: 1,
            configuration_binding: config.binding(),
            last_observed_unix_seconds: 0,
            highest_metadata: floors,
            root_chain: Vec::new(),
            metadata: BTreeMap::new(),
            releases: BTreeMap::new(),
            quarantined_app_sequences: BTreeSet::new(),
            quarantined_model_sequences: BTreeMap::new(),
        };
        let mut result = Self {
            directory: path.to_path_buf(),
            _lock: lock,
            state,
        };
        result.commit(result.state.clone())?;
        Ok(result)
    }

    /// Strict reopen. Missing or corrupt state blocks updates; it is never reset.
    pub fn open(path: &Path, config: &ProductionConfig) -> Result<Self> {
        check_directory(path)?;
        let lock = lock_directory(path)?;
        let state_path = path.join(STATE_FILE);
        check_regular(&state_path)?;
        let mut bytes = Vec::new();
        File::open(state_path)?
            .take(STATE_LIMIT + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > STATE_LIMIT {
            return Err(Error::State("state too large"));
        }
        let saved: Envelope =
            serde_json::from_slice(&bytes).map_err(|_| Error::State("malformed state"))?;
        if saved.sha256 != checksum(&saved.payload)? {
            return Err(Error::State("state checksum mismatch"));
        }
        validate_saved(&saved.payload, config)?;
        Ok(Self {
            directory: path.to_path_buf(),
            _lock: lock,
            state: saved.payload,
        })
    }

    pub fn snapshot(&self) -> &TrustState {
        &self.state
    }
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Invoke after a failed installation/health rollback. This does not lower
    /// trusted metadata or release floors and prevents a reinstall/rollback loop.
    pub fn quarantine_app_release(&mut self, sequence: u64) -> Result<()> {
        if sequence == 0
            || !self.state.releases.iter().any(|(scope, x)| {
                scope.split('/').count() == 4 && scope.ends_with("/app") && x.sequence >= sequence
            })
        {
            return Err(Error::Policy("cannot quarantine an unseen release"));
        }
        let mut next = self.state.clone();
        next.quarantined_app_sequences.insert(sequence);
        self.commit(next)
    }

    /// Quarantine only this model's authenticated release. Application and
    /// other-model floors/quarantine remain independent.
    pub fn quarantine_model_release(&mut self, model_id: &str, sequence: u64) -> Result<()> {
        crate::models::validate_model_id(model_id)?;
        let suffix = format!("/model/{model_id}");
        if sequence == 0
            || !self
                .state
                .releases
                .iter()
                .any(|(scope, floor)| scope.ends_with(&suffix) && floor.sequence >= sequence)
        {
            return Err(Error::Policy("cannot quarantine an unseen model release"));
        }
        let mut next = self.state.clone();
        next.quarantined_model_sequences
            .entry(model_id.into())
            .or_default()
            .insert(sequence);
        self.commit(next)
    }

    pub(crate) fn check_clock(&self, now: i64) -> Result<()> {
        if now <= 0 || now < self.state.last_observed_unix_seconds {
            return Err(Error::Rollback("wall clock moved backward or is invalid"));
        }
        Ok(())
    }

    pub(crate) fn commit(&mut self, next: TrustState) -> Result<()> {
        // A programming error cannot lower any persisted floor.
        if next.configuration_binding != self.state.configuration_binding
            || next.last_observed_unix_seconds < self.state.last_observed_unix_seconds
        {
            return Err(Error::Rollback("state replacement would lower trust"));
        }
        for (name, old) in &self.state.highest_metadata {
            let new = next
                .highest_metadata
                .get(name)
                .ok_or(Error::Rollback("metadata floor removed"))?;
            check_floor(Some(old), new)?;
        }
        for (name, old) in &self.state.releases {
            let new = next
                .releases
                .get(name)
                .ok_or(Error::Rollback("release floor removed"))?;
            if new.sequence < old.sequence
                || (new.sequence == old.sequence && new.identity != old.identity)
            {
                return Err(Error::Rollback("release state lowered or changed"));
            }
        }
        if !next
            .quarantined_app_sequences
            .is_superset(&self.state.quarantined_app_sequences)
        {
            return Err(Error::Rollback("failed-release quarantine removed"));
        }
        for (model_id, old) in &self.state.quarantined_model_sequences {
            if next
                .quarantined_model_sequences
                .get(model_id)
                .is_none_or(|new| !new.is_superset(old))
            {
                return Err(Error::Rollback("failed-model-release quarantine removed"));
            }
        }
        check_directory(&self.directory)?;
        let destination = self.directory.join(STATE_FILE);
        match fs::symlink_metadata(&destination) {
            Ok(_) => check_regular(&destination)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let saved = Envelope {
            sha256: checksum(&next)?,
            payload: next.clone(),
        };
        let bytes = serde_json::to_vec(&saved)?;
        if bytes.len() as u64 > STATE_LIMIT {
            return Err(Error::State("state size limit"));
        }
        let mut temporary = tempfile::Builder::new()
            .prefix(".trust-")
            .tempfile_in(&self.directory)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        persist_atomically(temporary, &destination)?;
        #[cfg(unix)]
        {
            File::open(&self.directory)?.sync_all()?;
        }
        self.state = next;
        Ok(())
    }
}

fn checksum(state: &TrustState) -> Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(state)?)))
}

pub(crate) fn floor<T: Role>(role: &T) -> Result<MetadataFloor> {
    if role.version().get() > MAX_METADATA_VERSION {
        return Err(Error::Policy("metadata version exceeds profile cap"));
    }
    Ok(MetadataFloor {
        version: role.version().get(),
        signed_sha256: hex::encode(Sha256::digest(
            role.canonical_form()
                .map_err(|e| Error::Tuf(e.to_string()))?,
        )),
    })
}

pub(crate) fn check_floor(old: Option<&MetadataFloor>, new: &MetadataFloor) -> Result<()> {
    if let Some(old) = old {
        if new.version < old.version {
            return Err(Error::Rollback("older metadata version"));
        }
        if new.version == old.version && new.signed_sha256 != old.signed_sha256 {
            return Err(Error::Rollback(
                "same metadata version has different signed content",
            ));
        }
    }
    Ok(())
}

pub(crate) fn validate_root_chain(
    state: &TrustState,
    config: &ProductionConfig,
) -> Result<Vec<Signed<Root>>> {
    if state.root_chain.len() > MAX_ROOT_CHAIN {
        return Err(Error::State("too many stored root rotations"));
    }
    let anchor: Signed<Root> = serde_json::from_slice(&config.pinned_root)?;
    anchor
        .signed
        .verify_role(&anchor)
        .map_err(|e| Error::Tuf(e.to_string()))?;
    let mut roots = vec![anchor];
    for value in &state.root_chain {
        let next: Signed<Root> = serde_json::from_value(value.clone())?;
        let previous = roots.last().expect("anchor present");
        if previous.signed.version.get().checked_add(1) != Some(next.signed.version.get())
            || next.signed.version.get() > MAX_METADATA_VERSION
            || !next.signed.consistent_snapshot
        {
            return Err(Error::State("nonsequential or unsupported root chain"));
        }
        previous
            .signed
            .verify_role(&next)
            .map_err(|e| Error::Tuf(e.to_string()))?;
        next.signed
            .verify_role(&next)
            .map_err(|e| Error::Tuf(e.to_string()))?;
        roots.push(next);
    }
    Ok(roots)
}

fn validate_saved(state: &TrustState, config: &ProductionConfig) -> Result<()> {
    if state.schema != 1
        || state.configuration_binding != config.binding()
        || state.last_observed_unix_seconds < 0
    {
        return Err(Error::State(
            "state schema or compiled trust binding mismatch",
        ));
    }
    let expected_scope = format!(
        "{}/{}/{}/app",
        crate::PRODUCT,
        config.platform(),
        config.channel()
    );
    for (scope, release) in &state.releases {
        if !(scope == &expected_scope
            || scope
                .strip_prefix(&format!(
                    "{}/{}/{}/model/",
                    crate::PRODUCT,
                    config.platform(),
                    config.channel()
                ))
                .is_some_and(|id| crate::models::validate_model_id(id).is_ok()))
            || release.sequence == 0
            || release.sequence > MAX_METADATA_VERSION
            || release.identity.len() != 64
            || !release.identity.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(Error::State("invalid persisted release identity"));
        }
    }
    if state.quarantined_app_sequences.iter().any(|sequence| {
        *sequence == 0
            || state
                .releases
                .get(&expected_scope)
                .is_none_or(|release| release.sequence < *sequence)
    }) {
        return Err(Error::State("invalid persisted quarantine"));
    }
    for (model_id, sequences) in &state.quarantined_model_sequences {
        crate::models::validate_model_id(model_id)?;
        let scope = format!(
            "{}/{}/{}/model/{model_id}",
            crate::PRODUCT,
            config.platform(),
            config.channel()
        );
        if sequences.is_empty()
            || sequences.iter().any(|sequence| {
                *sequence == 0
                    || state
                        .releases
                        .get(&scope)
                        .is_none_or(|release| release.sequence < *sequence)
            })
        {
            return Err(Error::State("invalid persisted model quarantine"));
        }
    }
    let roots = validate_root_chain(state, config)?;
    let root_floor = floor(&roots.last().expect("anchor present").signed)?;
    if state.highest_metadata.get("root") != Some(&root_floor) {
        return Err(Error::State("root floor disagrees with chain"));
    }
    for (name, entry) in &state.metadata {
        let root = roots
            .iter()
            .find(|root| root.signed.version.get() == entry.root_version)
            .ok_or(Error::State("role refers to unknown root"))?;
        let found = match name.as_str() {
            "timestamp" => validate_stored::<Timestamp>(root, entry)?,
            "snapshot" => validate_stored::<Snapshot>(root, entry)?,
            "targets" => validate_stored::<Targets>(root, entry)?,
            _ => return Err(Error::State("unknown persisted metadata role")),
        };
        if state.highest_metadata.get(name) != Some(&found) {
            return Err(Error::State("stored metadata floor mismatch"));
        }
    }
    if state.highest_metadata.len() != state.metadata.len() + 1 {
        return Err(Error::State("missing persisted role evidence"));
    }
    Ok(())
}

fn validate_stored<T>(root: &Signed<Root>, entry: &StoredMetadata) -> Result<MetadataFloor>
where
    T: Role + serde::de::DeserializeOwned,
{
    let signed: Signed<T> = serde_json::from_value(entry.envelope.clone())?;
    root.signed
        .verify_role(&signed)
        .map_err(|e| Error::Tuf(e.to_string()))?;
    floor(&signed.signed)
}

fn lock_directory(path: &Path) -> Result<File> {
    let name = path.join("trust.lock");
    match fs::symlink_metadata(&name) {
        Ok(_) => check_regular(&name)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(name)?;
    lock.try_lock().map_err(|e| match e {
        std::fs::TryLockError::WouldBlock => Error::Locked,
        std::fs::TryLockError::Error(error) => Error::Io(error),
    })?;
    Ok(lock)
}

pub(crate) fn check_regular(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || is_reparse(&metadata) {
        return Err(Error::State("non-regular or reparse-point file"));
    }
    Ok(())
}

pub(crate) fn check_directory(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        return Err(Error::State("state directory must be absolute"));
    }
    // Check every existing ancestor; canonicalization alone would follow links.
    for component in path.ancestors() {
        if component.as_os_str().is_empty() {
            continue;
        }
        let metadata = fs::symlink_metadata(component)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() || is_reparse(&metadata) {
            return Err(Error::State("non-directory or reparse-point ancestor"));
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = fs::metadata(path)?;
        if metadata.permissions().mode() & 0o022 != 0 {
            return Err(Error::State("state directory writable by other users"));
        }
    }
    Ok(())
}

fn is_reparse(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        let _ = metadata;
        false
    }
}

#[cfg(not(windows))]
fn persist_atomically(file: tempfile::NamedTempFile, destination: &Path) -> Result<()> {
    file.persist(destination).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn persist_atomically(file: tempfile::NamedTempFile, destination: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    // Close the synced temporary handle before rename. Same-directory paths keep
    // the rename on one volume. Reject embedded NUL before handing strings to Win32.
    let temporary = file.into_temp_path();
    let wide = |path: &Path| -> Result<Vec<u16>> {
        let mut value: Vec<u16> = path.as_os_str().encode_wide().collect();
        if value.contains(&0) {
            return Err(Error::State("NUL in persistent state path"));
        }
        value.push(0);
        Ok(value)
    };
    let source = wide(&temporary)?;
    let target = wide(destination)?;
    // SAFETY: both buffers live across the call and are NUL-terminated UTF-16;
    // their pointers are read-only. Paths were checked against reparse points.
    let success = unsafe {
        MoveFileExW(
            source.as_ptr(),
            target.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if success == 0 {
        return Err(Error::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}
