//! The executable Setup transaction. No installer success is a health success.
//! All production mutations are Windows-only; tests use scoped synthetic trees.
#![cfg_attr(not(windows), allow(dead_code))]
use super::{
    PRODUCT_ID, Result, UpdateError, integration, platform,
    recovery::{FileIdentity, HealthReport},
    storage,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};

#[path = "installer_retention.rs"]
mod retention;

const SCHEMA: u32 = 1;
const MAX_FILES: usize = 4096;
const MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const LAUNCHER: &str = "gloamere-image.exe";
const MARKER: &str = "LIGHT-IMAGE-INSTALL.txt";
const ACTIVE: &str = "installer-active.json";
const LAYOUT: &str = "installer-layout.json";
const LAUNCHER_STAMP: &str = "launcher.sha256";

fn refused(text: impl Into<String>) -> UpdateError {
    UpdateError::Security(text.into())
}
fn repair(text: impl Into<String>) -> UpdateError {
    UpdateError::Repair(text.into())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanFile {
    pub path: String,
    pub length: u64,
    pub sha256: String,
}
impl PlanFile {
    fn identity(&self) -> FileIdentity {
        FileIdentity {
            length: self.length,
            sha256: self.sha256.clone(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallPlan {
    pub schema: u32,
    pub product_id: String,
    pub version: String,
    pub launcher_protocol: u32,
    pub files: Vec<PlanFile>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Preparing,
    Prepared,
    Installing,
    PendingHealth,
    CommittingHealth,
    Committed,
    RollingBack,
    RolledBack,
    RepairRequired,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileRecord {
    path: String,
    before: Option<FileIdentity>,
    after: Option<FileIdentity>,
    permissions: Option<platform::FilePermissions>,
    generated: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Active {
    schema: u32,
    product_id: String,
    transaction: String,
    installation: PathBuf,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Layout {
    schema: u32,
    product_id: String,
    installation: PathBuf,
    version: String,
    launcher: FileIdentity,
    files: Vec<PlanFile>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema: u32,
    product_id: String,
    transaction: String,
    installation: PathBuf,
    version: String,
    previous_version: Option<String>,
    bootstrap: bool,
    phase: Phase,
    package: FileIdentity,
    helper: FileIdentity,
    parent: ProcessIdentity,
    files: Vec<FileRecord>,
    integration: integration::Snapshot,
    health_attempted: bool,
    failure: Option<String>,
    reboot_clear: bool,
    #[serde(default)]
    retention_link: Option<retention::Link>,
    #[serde(default)]
    retention_permissions: Option<BTreeMap<String, platform::FilePermissions>>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProcessIdentity {
    pid: u32,
    creation: u64,
    image: PathBuf,
}
#[derive(Debug, Clone)]
pub struct NativeHealthTicket {
    pub version: String,
    pub package_sha256: String,
    pub transaction: String,
}
#[derive(Debug, Clone)]
pub enum Startup {
    Ready,
    PendingHealth(NativeHealthTicket),
    RecoverySpawned,
}
#[derive(Debug, Clone)]
pub enum NativeOutcome {
    Committed(NativeHealthTicket),
    RolledBack(NativeHealthTicket),
}
#[cfg(test)]
thread_local! { static FAIL_AFTER: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) }; }
fn faultpoint() -> Result<()> {
    #[cfg(test)]
    {
        FAIL_AFTER.with(|remaining| match remaining.get() {
            Some(0) => {
                remaining.set(None);
                Err(std::io::Error::from_raw_os_error(28).into())
            }
            Some(n) => {
                remaining.set(Some(n - 1));
                Ok(())
            }
            None => Ok(()),
        })
    }
    #[cfg(not(test))]
    {
        Ok(())
    }
}
struct Transaction {
    directory: PathBuf,
    journal: Journal,
    _directory_pins: Vec<File>,
}

fn payload_path(value: &str) -> bool {
    matches!(
        value,
        LAUNCHER
            | "gloamere-image-worker.exe"
            | "light-image-tool.exe"
            | "light-provenance-worker.exe"
            | "runtime/gloamere-image-ui.exe"
            | "runtime/gloamere-image-worker.exe"
            | "assets/gloamere-image.ico"
            | "README.md"
            | "VALIDATION.md"
            | "LICENSE-MIT"
            | "LICENSE-APACHE"
            | "MANIFEST.json"
            | MARKER
    ) || value.starts_with("third-party-notices/")
}
fn generated_path(value: &str) -> bool {
    value.len() == 12
        && value.starts_with("unins")
        && value.as_bytes()[5..8].iter().all(u8::is_ascii_digit)
        && matches!(&value[8..], ".exe" | ".dat" | ".msg")
}
fn validate_identity(id: &FileIdentity) -> Result<()> {
    if id.length > MAX_BYTES
        || id.sha256.len() != 64
        || !id
            .sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(refused("Invalid bounded file identity"));
    }
    Ok(())
}
fn validate_files(files: &[PlanFile]) -> Result<()> {
    if files.is_empty() || files.len() > MAX_FILES {
        return Err(refused("Invalid file plan size"));
    }
    let mut names = BTreeSet::new();
    let mut total = 0u64;
    for file in files {
        storage::relative_path(&file.path)?;
        if !payload_path(&file.path) && !generated_path(&file.path) {
            return Err(refused("Plan contains a non-product file"));
        }
        validate_identity(&file.identity())?;
        if !names.insert(file.path.to_ascii_lowercase()) {
            return Err(refused("Duplicate/case-colliding payload path"));
        }
        total = total
            .checked_add(file.length)
            .ok_or_else(|| refused("Payload length overflow"))?;
    }
    if total > MAX_BYTES {
        return Err(refused("Expanded payload exceeds limit"));
    }
    for a in &names {
        for b in &names {
            if a != b && b.starts_with(&(a.to_owned() + "/")) {
                return Err(refused("File/directory payload collision"));
            }
        }
    }
    Ok(())
}
impl InstallPlan {
    fn validate(&self) -> Result<()> {
        if self.schema != SCHEMA
            || self.product_id != PRODUCT_ID
            || self.launcher_protocol != super::LAUNCHER_PROTOCOL
            || self.version != env!("CARGO_PKG_VERSION")
        {
            return Err(refused("Installer plan identity/version mismatch"));
        }
        validate_files(&self.files)?;
        for required in [
            LAUNCHER,
            "runtime/gloamere-image-ui.exe",
            "runtime/gloamere-image-worker.exe",
            MARKER,
            "MANIFEST.json",
        ] {
            if !self.files.iter().any(|f| f.path == required) {
                return Err(refused("Required native payload missing"));
            }
        }
        if self.files.iter().any(|f| {
            generated_path(&f.path)
                || matches!(
                    f.path.as_str(),
                    "gloamere-image-worker.exe"
                        | "light-image-tool.exe"
                        | "light-provenance-worker.exe"
                )
        }) {
            return Err(refused("New payload includes old/generated executable"));
        }
        Ok(())
    }
}
fn state_root() -> Result<PathBuf> {
    #[cfg(windows)]
    {
        native::local_app_data().map(|p| p.join("LightImageTool/updater-state"))
    }
    #[cfg(not(windows))]
    {
        Err(UpdateError::Deferred(
            "Native installation is Windows-only".into(),
        ))
    }
}
fn acquire_locks(state: &Path) -> Result<(storage::LockGuard, storage::LockGuard)> {
    let _state_pins = platform::ensure_update_directory(state)?;
    platform::verify_current_user_path(state)?;
    let maintenance = storage::LockGuard::acquire(&state.join("maintenance.lock"), false)?;
    let sessions = storage::LockGuard::acquire(&state.join("sessions.lock"), false)?;
    Ok((maintenance, sessions))
}
fn file_identity_optional(path: &Path) -> Result<Option<FileIdentity>> {
    storage::plain_path(path)?;
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_file() => Ok(Some(FileIdentity::read(path)?)),
        Ok(_) => Err(refused("Product path is not a regular file")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
fn copy_durable(
    source: &Path,
    destination: &Path,
    identity: &FileIdentity,
    replace: bool,
) -> Result<()> {
    storage::plain_path(source)?;
    storage::plain_path(destination)?;
    if FileIdentity::read(source)? != *identity {
        return Err(refused("Copy source identity changed"));
    }
    let parent = destination
        .parent()
        .ok_or_else(|| refused("No copy parent"))?;
    let _parent_pins = platform::ensure_private_data_directory(parent)?;
    let mut temp = platform::private_tempfile(parent)?;
    std::io::copy(&mut File::open(source)?, &mut temp)?;
    temp.flush()?;
    temp.as_file().sync_all()?;
    if FileIdentity::read(temp.path())? != *identity {
        return Err(refused("Copy readback mismatch"));
    }
    if replace {
        storage::persist_replace(temp, destination)?;
    } else {
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};
            let source: Vec<u16> = temp
                .path()
                .as_os_str()
                .encode_wide()
                .chain(Some(0))
                .collect();
            let target: Vec<u16> = destination
                .as_os_str()
                .encode_wide()
                .chain(Some(0))
                .collect();
            // No REPLACE_EXISTING: publishing a snapshot must never clobber.
            if unsafe { MoveFileExW(source.as_ptr(), target.as_ptr(), MOVEFILE_WRITE_THROUGH) } == 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
            drop(temp);
        }
        #[cfg(not(windows))]
        {
            temp.persist_noclobber(destination).map_err(|e| e.error)?;
        }
    }
    storage::sync_directory(parent)?;
    if FileIdentity::read(destination)? != *identity {
        return Err(refused("Durable copy readback mismatch"));
    }
    Ok(())
}
fn active(state: &Path, installation: &Path) -> Result<Option<Transaction>> {
    let path = state.join(ACTIVE);
    if !path.exists() {
        return Ok(None);
    }
    let pointer: Active = storage::read_json(&path)?;
    if pointer.schema != SCHEMA
        || pointer.product_id != PRODUCT_ID
        || pointer.installation != installation
        || !pointer.transaction.starts_with("installer-")
        || pointer.transaction.len() > 96
        || !pointer
            .transaction
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        || pointer.transaction.contains(['/', '\\'])
    {
        return Err(repair(
            "Active installer identity/path mismatch; preserve updater-state",
        ));
    }
    let directory = state.join(&pointer.transaction);
    if fs::read_to_string(directory.join("PRODUCT-OWNER.txt"))? != PRODUCT_ID {
        return Err(repair("Recovery owner mismatch"));
    }
    let journal: Journal = storage::read_json(&directory.join("journal.json"))?;
    // Bind the loaded journal before following its installation path or running recovery.
    if journal.installation != pointer.installation || journal.installation != installation {
        return Err(repair(
            "Active pointer/journal installation mismatch; preserve updater-state",
        ));
    }
    let mut pins = platform::pin_directories(&directory)?;
    let mut existing = journal.installation.as_path();
    while !existing.exists() {
        existing = existing
            .parent()
            .ok_or_else(|| refused("No existing installation ancestor"))?;
    }
    pins.extend(platform::pin_directories(existing)?);
    let txn = Transaction {
        directory,
        journal,
        _directory_pins: pins,
    };
    txn.validate()?;
    Ok(Some(txn))
}
fn previous_payload(
    installation: &Path,
    state: &Path,
) -> Result<(Option<String>, bool, Vec<PlanFile>)> {
    let marker = installation.join(MARKER);
    if !installation.exists() || fs::read_dir(installation)?.next().is_none() {
        return Ok((None, true, Vec::new()));
    }
    if fs::read_to_string(&marker)? != PRODUCT_ID {
        return Err(refused("Installation owner marker missing/mismatched"));
    }
    let layout_path = state.join(LAYOUT);
    if layout_path.exists() {
        let layout: Layout = storage::read_json(&layout_path)?;
        if layout.schema != SCHEMA
            || layout.product_id != PRODUCT_ID
            || layout.installation != installation
            || FileIdentity::read(&installation.join(LAUNCHER))? != layout.launcher
        {
            return Err(refused("Stable launcher ownership or identity mismatch"));
        }
        validate_files(&layout.files)?;
        return Ok((Some(layout.version), false, layout.files));
    }
    // The one bounded legacy bootstrap. Manifest supplies inventory only: each
    // byte is checked and ownership must independently match before using it.
    let manifest: serde_json::Value = storage::read_json(&installation.join("MANIFEST.json"))?;
    if manifest["product"] != "gloamere-image" || manifest["version"] != "0.6.0" {
        return Err(refused(
            "Only the owned v0.6.0 bootstrap is supported without a stable layout",
        ));
    }
    let rows = manifest["files"]
        .as_array()
        .ok_or_else(|| refused("Legacy manifest files missing"))?;
    let mut files = Vec::new();
    for row in rows {
        files.push(PlanFile {
            path: row["path"]
                .as_str()
                .ok_or_else(|| refused("Legacy path missing"))?
                .into(),
            length: row["size_bytes"]
                .as_u64()
                .ok_or_else(|| refused("Legacy size missing"))?,
            sha256: row["sha256"]
                .as_str()
                .ok_or_else(|| refused("Legacy hash missing"))?
                .into(),
        });
    }
    for name in [MARKER, "MANIFEST.json"] {
        let id = FileIdentity::read(&installation.join(name))?;
        files.push(PlanFile {
            path: name.into(),
            length: id.length,
            sha256: id.sha256,
        });
    }
    validate_files(&files)?;
    if !files.iter().any(|f| f.path == LAUNCHER)
        || !files.iter().any(|f| f.path == "gloamere-image-worker.exe")
    {
        return Err(refused("Legacy executable pair is incomplete"));
    }
    Ok((Some("0.6.0".into()), true, files))
}
fn reconcile_previous(state: &Path, installation: &Path) -> Result<()> {
    let Some(mut previous) = active(state, installation)? else {
        return Ok(());
    };
    match previous.journal.phase {
        Phase::Committed | Phase::RolledBack => Ok(()),
        Phase::Preparing | Phase::Prepared | Phase::Installing | Phase::RollingBack => {
            previous.rollback("Previous Setup was interrupted; verified recovery before retry")
        }
        _ => Err(repair(
            "Previous installation awaits health or requires explicit repair; no new installation started",
        )),
    }
}
impl Transaction {
    fn validate(&self) -> Result<()> {
        let j = &self.journal;
        storage::plain_path(&self.directory)?;
        storage::plain_path(&j.installation)?;
        if j.schema != SCHEMA
            || j.product_id != PRODUCT_ID
            || self.directory.file_name().and_then(|n| n.to_str()) != Some(&j.transaction)
            || self.directory.starts_with(&j.installation)
            || j.files.len() > MAX_FILES
        {
            return Err(repair("Unsupported/inconsistent installer journal"));
        }
        validate_identity(&j.package)?;
        validate_identity(&j.helper)?;
        let mut seen = BTreeSet::new();
        for f in &j.files {
            storage::relative_path(&f.path)?;
            if !(payload_path(&f.path) || generated_path(&f.path))
                || !seen.insert(f.path.to_lowercase())
                || f.generated != generated_path(&f.path)
            {
                return Err(repair("Invalid recovery path"));
            }
            for id in [&f.before, &f.after].into_iter().flatten() {
                validate_identity(id)?;
            }
        }
        j.integration.validate(&j.installation, &self.directory)?;
        Ok(())
    }
    fn persist(&self) -> Result<()> {
        self.validate()?;
        storage::atomic_json(&self.directory.join("journal.json"), &self.journal)?;
        let reread: Journal = storage::read_json(&self.directory.join("journal.json"))?;
        if reread.phase != self.journal.phase {
            return Err(repair("Journal readback mismatch"));
        }
        Ok(())
    }
    fn verify_backups(&self) -> Result<()> {
        for f in &self.journal.files {
            if let Some(id) = &f.before
                && FileIdentity::read(&storage::owned_join(
                    &self.directory.join("files"),
                    &f.path,
                )?)? != *id
            {
                return Err(repair("Recovery backup mismatch; preserve evidence"));
            }
        }
        Ok(())
    }
    fn verify_target(&self) -> Result<()> {
        for f in &self.journal.files {
            let live = storage::owned_join(&self.journal.installation, &f.path)?;
            if file_identity_optional(&live)? != f.after {
                return Err(repair(format!("Installed file mismatch: {}", f.path)));
            }
        }
        Ok(())
    }
    fn prepare(
        state: &Path,
        installation: &Path,
        plan: InstallPlan,
        parent: ProcessIdentity,
        helper: &Path,
        package: &Path,
        explorer: bool,
    ) -> Result<Self> {
        Self::prepare_with_reboot_probe(
            state,
            installation,
            plan,
            parent,
            helper,
            package,
            explorer,
            platform::reboot_operations_clear,
        )
    }
    // Keep the real production probe as the default; injection is private and
    // allows tests to exercise failure before any new transaction publication.
    #[allow(clippy::too_many_arguments)]
    fn prepare_with_reboot_probe(
        state: &Path,
        installation: &Path,
        plan: InstallPlan,
        parent: ProcessIdentity,
        helper: &Path,
        package: &Path,
        explorer: bool,
        mut reboot_probe: impl FnMut() -> Result<bool>,
    ) -> Result<Self> {
        plan.validate()?;
        storage::plain_path(installation)?;
        storage::plain_path(state)?;
        if state.starts_with(installation) {
            return Err(refused("Recovery state must be outside installation"));
        }
        reconcile_previous(state, installation)?;
        // An initial refusal must not publish Preparing or allocate backup sets.
        // Keep both the post-snapshot check below and begin()'s pre-mutation check.
        match reboot_probe() {
            Ok(true) => {}
            Ok(false) => {
                return Err(UpdateError::Deferred(
                    "Pending restart operations; no new snapshot or installation started".into(),
                ));
            }
            Err(error) => {
                return Err(UpdateError::Deferred(format!(
                    "Restart-state preflight could not be verified: {error}; no new snapshot or installation started"
                )));
            }
        }
        let retention_link = Some(retention::Link {
            previous_terminal: active(state, installation)?.map(|old| old.journal.transaction),
        });
        let (version, bootstrap, old) = previous_payload(installation, state)?;
        if installation.exists() {
            platform::verify_current_user_path(installation)?;
        } else {
            let mut parent = installation
                .parent()
                .ok_or_else(|| refused("Missing install parent"))?;
            while !parent.exists() {
                parent = parent
                    .parent()
                    .ok_or_else(|| refused("No install ancestor"))?;
            }
            platform::verify_current_user_path(parent)?;
        }
        let helper_id = FileIdentity::read(helper)?;
        if plan
            .files
            .iter()
            .find(|f| f.path == LAUNCHER)
            .map(PlanFile::identity)
            != Some(helper_id.clone())
        {
            return Err(refused(
                "Extracted helper differs from embedded launcher identity",
            ));
        }
        let mut old: BTreeMap<_, _> = old
            .into_iter()
            .map(|f| (f.path.clone(), f.identity()))
            .collect();
        if installation.exists() {
            for name in integration::owned_uninstaller_files(installation)? {
                old.insert(name.clone(), FileIdentity::read(&installation.join(name))?);
            }
        }
        if installation.exists() {
            for entry in fs::read_dir(installation)? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if generated_path(&name) && !old.contains_key(&name) {
                    return Err(refused("Unowned generated-uninstaller path already exists"));
                }
            }
        }
        let mut after: BTreeMap<_, _> = plan
            .files
            .iter()
            .map(|f| (f.path.clone(), f.identity()))
            .collect();
        if !bootstrap {
            after.insert(
                LAUNCHER.into(),
                old.get(LAUNCHER)
                    .ok_or_else(|| refused("Stable launcher absent from prior owned layout"))?
                    .clone(),
            );
        }
        for (name, id) in &old {
            if generated_path(name) {
                after.insert(name.clone(), id.clone());
            }
        }
        let all: BTreeSet<_> = old.keys().chain(after.keys()).cloned().collect();
        if all.len() > MAX_FILES {
            return Err(refused("Combined file plan too large"));
        }
        let mut files = Vec::new();
        let mut backup_bytes = 0u64;
        for path in all {
            let live = storage::owned_join(installation, &path)?;
            let before = file_identity_optional(&live)?;
            if before.as_ref() != old.get(&path) {
                return Err(refused(format!(
                    "Foreign, changed or missing owned file: {path}"
                )));
            }
            if before.is_some() {
                native_file_unlocked(&live)?;
            }
            let permissions = if before.is_some() {
                Some(platform::capture_permissions(&live)?)
            } else {
                None
            };
            backup_bytes = backup_bytes
                .checked_add(before.as_ref().map_or(0, |v| v.length))
                .ok_or_else(|| refused("Backup size overflow"))?;
            files.push(FileRecord {
                generated: generated_path(&path),
                path: path.clone(),
                before,
                after: after.get(&path).cloned(),
                permissions,
            });
        }
        let install_bytes = plan.files.iter().map(|f| f.length).sum::<u64>();
        let largest_old = files
            .iter()
            .filter_map(|f| f.before.as_ref().map(|id| id.length))
            .max()
            .unwrap_or(0);
        let required = space_budget(backup_bytes, install_bytes, largest_old, helper_id.length)?;
        // Conservative combined budget on both volumes also covers the common
        // same-volume case; do not independently double-spend the same free bytes.
        check_space(state, required)?;
        check_space(installation, required)?;
        let transaction = format!(
            "installer-{}-{}",
            parent.pid,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| refused("System time invalid"))?
                .as_nanos()
        );
        let directory = state.join(&transaction);
        let _new_directory_pins = platform::create_private_data_directory_new(&directory)?;
        storage::atomic_bytes(&directory.join("PRODUCT-OWNER.txt"), PRODUCT_ID.as_bytes())?;
        let snapshot =
            integration::Snapshot::capture(installation, &directory, explorer, &plan.version)?;
        let mut pins = platform::pin_directories(&directory)?;
        let mut existing = installation;
        while !existing.exists() {
            existing = existing
                .parent()
                .ok_or_else(|| refused("No existing installation ancestor"))?;
        }
        pins.extend(platform::pin_directories(existing)?);
        let mut txn = Self {
            directory,
            _directory_pins: pins,
            journal: Journal {
                schema: SCHEMA,
                product_id: PRODUCT_ID.into(),
                transaction: transaction.clone(),
                installation: installation.into(),
                version: plan.version,
                previous_version: version,
                bootstrap,
                phase: Phase::Preparing,
                package: FileIdentity::read(package)?,
                helper: helper_id.clone(),
                parent,
                files,
                integration: snapshot,
                health_attempted: false,
                failure: None,
                reboot_clear: false,
                retention_link,
                retention_permissions: None,
            },
        };
        txn.persist()?;
        faultpoint()?;
        storage::atomic_json(
            &state.join(ACTIVE),
            &Active {
                schema: SCHEMA,
                product_id: PRODUCT_ID.into(),
                transaction,
                installation: installation.into(),
            },
        )?;
        faultpoint()?;
        copy_durable(
            helper,
            &txn.directory.join("recovery-helper.exe"),
            &helper_id,
            false,
        )?;
        faultpoint()?;
        for f in &txn.journal.files {
            if let Some(id) = &f.before {
                copy_durable(
                    &storage::owned_join(installation, &f.path)?,
                    &storage::owned_join(&txn.directory.join("files"), &f.path)?,
                    id,
                    false,
                )?;
                faultpoint()?;
            }
        }
        txn.verify_backups()?;
        txn.journal.retention_permissions = Some(retention::snapshot_permissions(&txn.directory)?);
        if !reboot_probe()? {
            return Err(repair(
                "Pending/unknown restart operations; Setup did not modify payload",
            ));
        }
        txn.journal.reboot_clear = true;
        txn.journal.phase = Phase::Prepared;
        txn.persist()?;
        faultpoint()?;
        Ok(txn)
    }
    fn begin(&mut self) -> Result<()> {
        if self.journal.phase != Phase::Prepared {
            return Err(refused("Setup transaction is not prepared"));
        }
        self.verify_backups()?;
        for f in &self.journal.files {
            let p = storage::owned_join(&self.journal.installation, &f.path)?;
            if file_identity_optional(&p)? != f.before {
                return Err(refused("Payload changed after snapshot"));
            }
            if p.exists() {
                native_file_unlocked(&p)?;
            }
        }
        if !platform::reboot_operations_clear()? {
            return Err(repair("Restart operations appeared before install"));
        }
        let target = self
            .journal
            .files
            .iter()
            .filter_map(|f| f.after.as_ref().map(|id| id.length))
            .sum::<u64>();
        let largest = self
            .journal
            .files
            .iter()
            .filter_map(|f| f.before.as_ref().map(|id| id.length))
            .max()
            .unwrap_or(0);
        let required = space_budget(0, target, largest, 0)?;
        check_space(&self.journal.installation, required)?;
        check_space(&self.directory, required)?;
        self.journal.integration.mark_write_intent()?;
        self.journal.phase = Phase::Installing;
        self.persist()?;
        faultpoint()?;
        // Install the only recovery entry atomically, before any other payload
        // changes. Until this succeeds the complete old root pair stays intact.
        if self.journal.bootstrap {
            let root = self.journal.installation.join(LAUNCHER);
            let id = self
                .journal
                .files
                .iter()
                .find(|f| f.path == LAUNCHER)
                .and_then(|f| f.after.as_ref())
                .ok_or_else(|| refused("Missing bootstrap launcher"))?;
            copy_durable(
                &self.directory.join("recovery-helper.exe"),
                &root,
                id,
                root.exists(),
            )?;
            faultpoint()?;
        }
        self._directory_pins
            .extend(platform::pin_directories(&self.journal.installation)?);
        self.journal
            .integration
            .remove_legacy_shortcut(&self.journal.installation, &self.directory)?;
        Ok(())
    }
    fn pending_health(&mut self) -> Result<()> {
        if self.journal.phase != Phase::Installing {
            return Err(refused("Setup state is not installing"));
        }
        if !platform::reboot_operations_clear()? {
            return self.block("Pending or unknown restart operations; no automatic restore");
        }
        // Removed legacy files are known preimages, never wildcard deletion.
        for f in &self.journal.files {
            if f.after.is_none() {
                let path = storage::owned_join(&self.journal.installation, &f.path)?;
                if file_identity_optional(&path)? != f.before {
                    return self.block("Obsolete file changed during install");
                }
                if path.exists() {
                    fs::remove_file(&path)?;
                    storage::sync_directory(path.parent().unwrap())?;
                }
            }
        }
        // Inno-generated files cannot have a precomputed target hash. Only
        // capture the exact registered owned uninstaller and fixed siblings,
        // after the complete registration/shortcut postconditions validate.
        self.journal
            .integration
            .seal_after(&self.journal.installation, &self.directory)?;
        let owned = integration::owned_uninstaller_files(&self.journal.installation)?;
        for entry in fs::read_dir(&self.journal.installation)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if generated_path(&name) && !owned.contains(&name) {
                return self.block("Unclaimed generated file appeared during Setup; preserve it");
            }
        }
        for record in self.journal.files.iter_mut().filter(|f| f.generated) {
            record.after = None;
        }
        for name in owned {
            let id = FileIdentity::read(&self.journal.installation.join(&name))?;
            if let Some(record) = self.journal.files.iter_mut().find(|f| f.path == name) {
                record.after = Some(id);
            } else {
                self.journal.files.push(FileRecord {
                    path: name,
                    before: None,
                    after: Some(id),
                    permissions: None,
                    generated: true,
                });
            }
        }
        self.verify_target()?;
        self.journal.phase = Phase::PendingHealth;
        self.persist()?;
        faultpoint()
    }
    fn block<T>(&mut self, message: &str) -> Result<T> {
        self.journal.phase = Phase::RepairRequired;
        self.journal.failure = Some(message.chars().take(1024).collect());
        self.persist()?;
        Err(repair(message))
    }
    fn rollback(&mut self, reason: &str) -> Result<()> {
        if matches!(
            self.journal.phase,
            Phase::Committed | Phase::CommittingHealth | Phase::RepairRequired
        ) {
            return Err(repair("Transaction cannot be automatically rolled back"));
        }
        if self.journal.phase == Phase::Preparing || self.journal.phase == Phase::Prepared {
            self.journal.phase = Phase::RolledBack;
            self.journal.failure = Some(reason.into());
            return self.persist();
        }
        if !self.journal.reboot_clear || !platform::reboot_operations_clear()? {
            return self.block("Restart operations cannot be excluded; retain snapshot for repair");
        }
        self.verify_backups()?;
        for entry in fs::read_dir(&self.journal.installation)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if generated_path(&name) && !self.journal.files.iter().any(|f| f.path == name) {
                return self.block("Unsealed new uninstaller file retained; repair required");
            }
        }
        // Preflight every effect before changing any, including missing files.
        for f in &self.journal.files {
            let current =
                file_identity_optional(&storage::owned_join(&self.journal.installation, &f.path)?)?;
            if f.before.is_none() && current != f.before && current != f.after {
                return self.block("Unknown new file bytes; retained full snapshot for repair");
            }
        }
        if let Err(error) = self
            .journal
            .integration
            .preflight_restore(&self.journal.installation, &self.directory)
        {
            return self.block(&format!("Integration recovery preflight failed: {error}"));
        }
        self.journal.phase = Phase::RollingBack;
        self.journal.failure = Some(reason.chars().take(1024).collect());
        self.persist()?;
        // Root must remain the recovery-capable launcher until every other
        // file, registry value and shortcut has durable old-state readback.
        for f in self.journal.files.iter().filter(|f| f.path != LAUNCHER) {
            self.restore_file(f)?;
            faultpoint()?;
        }
        self.journal
            .integration
            .restore(&self.journal.installation, &self.directory)?;
        faultpoint()?;
        if let Some(root) = self.journal.files.iter().find(|f| f.path == LAUNCHER) {
            self.restore_file(root)?;
        }
        faultpoint()?;
        self.journal.phase = Phase::RolledBack;
        self.persist()
    }
    fn restore_file(&self, f: &FileRecord) -> Result<()> {
        let live = storage::owned_join(&self.journal.installation, &f.path)?;
        if let Some(id) = &f.before {
            if file_identity_optional(&live)?.as_ref() != Some(id) {
                copy_durable(
                    &storage::owned_join(&self.directory.join("files"), &f.path)?,
                    &live,
                    id,
                    true,
                )?;
            }
            if let Some(p) = &f.permissions {
                platform::restore_permissions(&live, p)?;
                let now = platform::capture_permissions(&live)?;
                if serde_json::to_vec(&now)? != serde_json::to_vec(p)? {
                    return Err(repair("Restored file permissions differ"));
                }
            }
        } else if live.exists() {
            fs::remove_file(&live)?;
            storage::sync_directory(live.parent().unwrap())?;
        }
        if file_identity_optional(&live)? != f.before {
            return Err(repair("Restored file readback mismatch"));
        }
        Ok(())
    }
    fn ticket(&self) -> NativeHealthTicket {
        NativeHealthTicket {
            version: self.journal.version.clone(),
            package_sha256: self.journal.package.sha256.clone(),
            transaction: self.journal.transaction.clone(),
        }
    }
    fn finish_commit(&mut self, state: &Path) -> Result<()> {
        self.verify_target()?;
        let launcher = FileIdentity::read(&self.journal.installation.join(LAUNCHER))?;
        let files = self
            .journal
            .files
            .iter()
            .filter_map(|f| {
                f.after.as_ref().map(|id| PlanFile {
                    path: f.path.clone(),
                    length: id.length,
                    sha256: id.sha256.clone(),
                })
            })
            .collect();
        storage::atomic_json(
            &state.join(LAYOUT),
            &Layout {
                schema: SCHEMA,
                product_id: PRODUCT_ID.into(),
                installation: self.journal.installation.clone(),
                version: self.journal.version.clone(),
                launcher,
                files,
            },
        )?;
        let launcher_stamp = format!(
            "{PRODUCT_ID}\n{}\n",
            FileIdentity::read(&self.journal.installation.join(LAUNCHER))?.sha256
        );
        storage::atomic_bytes(&state.join(LAUNCHER_STAMP), launcher_stamp.as_bytes())?;
        self.journal.phase = Phase::Committed;
        self.persist()?;
        retention::best_effort(state, &self.journal);
        Ok(())
    }
}

fn space_budget(backup: u64, target: u64, largest_restore: u64, helper: u64) -> Result<u64> {
    [backup, target, largest_restore, helper, 64 * 1024 * 1024]
        .into_iter()
        .try_fold(0u64, |total, value| {
            total
                .checked_add(value)
                .ok_or_else(|| refused("Space budget overflow"))
        })
}
fn check_space(path: &Path, required: u64) -> Result<()> {
    #[cfg(windows)]
    {
        native::check_space(path, required)
    }
    #[cfg(not(windows))]
    {
        let _ = (path, required);
        Ok(())
    } // Controlled fixtures only; run() rejects non-Windows.
}
fn native_file_unlocked(path: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let _file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(path)?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Ok(())
    }
}
fn recovery_start(mut txn: Transaction, state: &Path, reason: &str) -> Result<Startup> {
    if txn.journal.bootstrap
        && txn
            .journal
            .files
            .iter()
            .any(|f| f.path == LAUNCHER && f.before != f.after)
        && std::env::current_exe()?.starts_with(&txn.journal.installation)
    {
        let helper = txn.directory.join("recovery-helper.exe");
        if FileIdentity::read(&helper)? != txn.journal.helper {
            return txn.block("Recovery helper changed; preserve snapshot");
        }
        #[cfg(windows)]
        {
            let identity = native::process_identity(std::process::id())?;
            storage::atomic_json(&txn.directory.join("recovery-parent.json"), &identity)?;
            if let Some(permissions) = &mut txn.journal.retention_permissions {
                permissions.insert(
                    "recovery-parent.json".into(),
                    platform::capture_permissions(&txn.directory.join("recovery-parent.json"))?,
                );
                txn.persist()?;
            }
            native::spawn_recovery(
                &helper,
                &txn.journal.installation,
                &identity,
                &txn.journal.transaction,
            )?;
            return Ok(Startup::RecoverySpawned);
        }
        #[cfg(not(windows))]
        {
            let _ = helper;
            return Err(repair(
                "Bootstrap root replacement requires external Windows recovery helper",
            ));
        }
    }
    let _ = state;
    txn.rollback(reason)?;
    Ok(Startup::Ready)
}
pub fn recover_before_launch(installation: &Path) -> Result<Startup> {
    let state = state_root()?;
    let _state_pins = platform::ensure_update_directory(&state)?;
    platform::verify_current_user_path(&state)?;
    let _maintenance = storage::LockGuard::acquire(&state.join("maintenance.lock"), false)?;
    recover_before_launch_locked(installation)
}
/// Caller owns maintenance.lock continuously through its subsequent shared
/// session admission. This closes the first-health concurrent-launch gap.
pub fn recover_before_launch_locked(installation: &Path) -> Result<Startup> {
    let state = state_root()?;
    let Some(mut txn) = active(&state, installation)? else {
        return Ok(Startup::Ready);
    };
    if matches!(txn.journal.phase, Phase::Committed | Phase::RolledBack) {
        retention::best_effort(&state, &txn.journal);
        return Ok(Startup::Ready);
    }
    let _sessions = storage::LockGuard::acquire(&state.join("sessions.lock"), false)?;
    match txn.journal.phase {
        Phase::Committed | Phase::RolledBack => Ok(Startup::Ready),
        Phase::RepairRequired => Err(repair(
            txn.journal
                .failure
                .unwrap_or_else(|| "Installation repair required".into()),
        )),
        Phase::CommittingHealth => {
            txn.finish_commit(&state)?;
            Ok(Startup::Ready)
        }
        Phase::PendingHealth if !txn.journal.health_attempted => {
            if !platform::reboot_operations_clear()? {
                return txn.block(
                    "Pending restart operations detected before health launch; preserve snapshot",
                );
            }
            txn.verify_target()?;
            txn.journal.health_attempted = true;
            txn.persist()?;
            Ok(Startup::PendingHealth(txn.ticket()))
        }
        _ => recovery_start(txn, &state, "Interrupted or failed native installation"),
    }
}
/// Caller owns maintenance.lock until its shared session lock is acquired.
/// This admission check performs no mutation and cannot advance health state.
pub fn runtime_admission(installation: &Path, health_transaction: Option<&str>) -> Result<()> {
    let state = state_root()?;
    let Some(txn) = active(&state, installation)? else {
        return if health_transaction.is_none() {
            Ok(())
        } else {
            Err(refused("No matching health transaction"))
        };
    };
    runtime_state_admission(&txn.journal, health_transaction)
}
fn runtime_state_admission(journal: &Journal, health_transaction: Option<&str>) -> Result<()> {
    match journal.phase {
        Phase::Committed
            if health_transaction.is_none()
                || health_transaction == Some(journal.transaction.as_str()) =>
        {
            Ok(())
        }
        Phase::RolledBack if health_transaction.is_none() => Ok(()),
        Phase::PendingHealth
            if journal.health_attempted
                && health_transaction == Some(journal.transaction.as_str()) =>
        {
            Ok(())
        }
        _ => Err(repair(
            "Runtime entry blocked by incomplete installer transaction",
        )),
    }
}
pub fn last_outcome(installation: &Path) -> Result<Option<NativeOutcome>> {
    let state = state_root()?;
    let _lock = storage::LockGuard::acquire(&state.join("maintenance.lock"), false)?;
    let Some(txn) = active(&state, installation)? else {
        return Ok(None);
    };
    Ok(match txn.journal.phase {
        Phase::Committed => Some(NativeOutcome::Committed(txn.ticket())),
        Phase::RolledBack => Some(NativeOutcome::RolledBack(txn.ticket())),
        _ => None,
    })
}
pub fn record_setup_exit(installation: &Path, code: i32) -> Result<()> {
    let state = state_root()?;
    let _locks = acquire_locks(&state)?;
    let mut txn = active(&state, installation)?
        .ok_or_else(|| repair("Setup returned without a persistent transaction"))?;
    if code == 3010 || !platform::reboot_operations_clear()? {
        return txn.block(
            "Setup requested restart or pending rename operations are ambiguous; preserve snapshot",
        );
    }
    if code != 0 {
        if txn.journal.phase == Phase::RolledBack {
            return Ok(());
        }
        return txn.rollback(&format!("Setup exited with {code}"));
    }
    if !matches!(txn.journal.phase, Phase::PendingHealth | Phase::Committed) {
        return txn.block("Setup exit zero lacks a validated pending-health journal");
    }
    Ok(())
}
pub fn fail_health(installation: &Path, reason: &str) -> Result<Startup> {
    let state = state_root()?;
    let _locks = acquire_locks(&state)?;
    let txn = active(&state, installation)?
        .ok_or_else(|| repair("Health failure has no native transaction"))?;
    recovery_start(txn, &state, reason)
}
pub fn confirm_health(
    installation: &Path,
    ticket: &NativeHealthTicket,
    report: &HealthReport,
) -> Result<()> {
    let state = state_root()?;
    let _lock = storage::LockGuard::acquire(&state.join("maintenance.lock"), false)?;
    let mut txn = active(&state, installation)?
        .ok_or_else(|| repair("Health report has no native transaction"))?;
    if txn.journal.phase != Phase::PendingHealth
        || !txn.journal.health_attempted
        || txn.journal.transaction != ticket.transaction
        || report.version != txn.journal.version
        || report.version != ticket.version
        || report.package_sha256 != txn.journal.package.sha256
        || report.package_sha256 != ticket.package_sha256
        || report.launcher_protocol != super::LAUNCHER_PROTOCOL
        || report.updater_protocol != super::UPDATER_PROTOCOL
        || report.worker_protocol != crate::ai::model::WORKER_PROTOCOL
        || !report.settings_readable
        || !report.built_in_decode
        || !report.ui_event_loop
    {
        return Err(refused("Native health report is incomplete or mismatched"));
    }
    if !platform::reboot_operations_clear()? {
        return txn
            .block("Pending restart operations detected before health commit; preserve snapshot");
    }
    txn.verify_target()?;
    txn.journal.phase = Phase::CommittingHealth;
    txn.persist()?;
    txn.finish_commit(&state)
}

// Only actual kernel lock contention is evidence of a delegated lock. A path,
// owner, ACL or I/O error must never be reclassified as "Setup holds the lock".
#[cfg(any(windows, test))]
fn require_delegated_lock(probe: Result<storage::LockGuard>) -> Result<()> {
    match probe {
        Err(UpdateError::Deferred(_)) => Ok(()),
        Err(error) => Err(error),
        Ok(_) => Err(refused("Setup did not hold both required locks")),
    }
}

// Fixed bounded child-exit diagnostics work even when the state directory is
// rejected and cannot safely receive a diagnostic file. They never authorize recovery.
#[cfg(any(windows, test))]
fn security_diagnostic_exit(error: &UpdateError) -> Option<i32> {
    match error {
        UpdateError::PathOwnerMismatch => Some(41),
        UpdateError::PathUnsafeWriteAccess => Some(42),
        _ => None,
    }
}

/// Fixed local command protocol. It never accepts arbitrary executable commands,
/// registry paths, state roots or cleanup directories from the caller.
pub fn run(args: Vec<OsString>) -> Result<i32> {
    #[cfg(not(windows))]
    {
        let _ = args;
        Err(UpdateError::Deferred(
            "Native installer commands require Windows".into(),
        ))
    }
    #[cfg(windows)]
    {
        let result = run_windows(args);
        if let Err(error) = &result
            && let Ok(state) = state_root()
            && storage::plain_path(&state).is_ok()
            && state.exists()
            && platform::verify_current_user_path(&state).is_ok()
        {
            let text = format!(
                "Native installer stopped safely: {error}\nSnapshots were not deleted. Do not bypass OS warnings or restart to force file replacement.\n"
            );
            let _ = storage::atomic_bytes(&state.join("last-installer-error.txt"), text.as_bytes());
        }
        if let Err(error) = &result
            && let Some(code) = security_diagnostic_exit(error)
        {
            // The numeric exit status is the authoritative diagnostic protocol.
            // No optional output sink can replace it with a write failure or panic.
            return Ok(code);
        }
        result
    }
}
#[cfg(windows)]
fn run_windows(args: Vec<OsString>) -> Result<i32> {
    if args.is_empty() {
        return Err(refused("Missing native operation"));
    }
    let operation = args[0]
        .to_str()
        .ok_or_else(|| refused("Invalid operation"))?;
    let mut options = BTreeMap::new();
    let (chunks, remainder) = args[1..].as_chunks::<2>();
    for pair in chunks {
        let key = pair[0].to_str().ok_or_else(|| refused("Invalid option"))?;
        if !matches!(
            key,
            "--install-dir" | "--plan" | "--package" | "--parent-pid" | "--explorer"
        ) || options.insert(key, pair[1].clone()).is_some()
        {
            return Err(refused("Unknown or duplicate installer option"));
        }
    }
    if !remainder.is_empty() {
        return Err(refused("Incomplete installer option"));
    }
    let installation = PathBuf::from(
        options
            .get("--install-dir")
            .ok_or_else(|| refused("Missing install directory"))?,
    );
    storage::plain_path(&installation)?;
    let parent_pid = options
        .get("--parent-pid")
        .and_then(|p| p.to_str())
        .and_then(|p| p.parse::<u32>().ok())
        .ok_or_else(|| refused("Missing parent process identity"))?;
    let parent = native::process_identity(parent_pid)?;
    if native::parent_pid()? != parent_pid {
        return Err(refused("Caller is not the declared Setup parent"));
    }
    let state = state_root()?;
    let _state_pins = platform::ensure_update_directory(&state)?;
    platform::verify_current_user_path(&state)?;
    if operation == "recover-detached" {
        let txn = active(&state, &installation)?
            .ok_or_else(|| repair("Detached recovery has no active transaction"))?;
        if FileIdentity::read(&std::env::current_exe()?)? != txn.journal.helper {
            return Err(refused("Detached helper identity mismatch"));
        }
        let expected: ProcessIdentity =
            storage::read_json(&txn.directory.join("recovery-parent.json"))?;
        if expected.pid != parent_pid
            || native::process_identity(parent_pid)? != expected
            || native::parent_pid()? != parent_pid
        {
            return Err(refused("Detached recovery parent mismatch"));
        }
        native::wait_for_process(&expected, &txn.journal.transaction)?;
        let _locks = acquire_locks(&state)?;
        let mut txn = active(&state, &installation)?
            .ok_or_else(|| repair("Recovery transaction vanished"))?;
        txn.rollback("Bootstrap health or installation failure")?;
        return Ok(0);
    }
    // Setup owns both locks across all commands. Probe must see contention.
    require_delegated_lock(storage::LockGuard::acquire(
        &state.join("maintenance.lock"),
        false,
    ))?;
    require_delegated_lock(storage::LockGuard::acquire(
        &state.join("sessions.lock"),
        false,
    ))?;
    if operation == "uninstall-remove-launcher" {
        let layout: Layout = storage::read_json(&state.join(LAYOUT))?;
        if layout.schema != SCHEMA
            || layout.product_id != PRODUCT_ID
            || layout.installation != installation
            || file_identity_optional(&installation.join(LAUNCHER))?
                .is_some_and(|id| id != layout.launcher)
            || FileIdentity::read(&std::env::current_exe()?)? != layout.launcher
        {
            return Err(refused("Uninstall launcher identity mismatch"));
        }
        if installation.join(LAUNCHER).exists() {
            fs::remove_file(installation.join(LAUNCHER))?;
        }
        if installation.exists() {
            storage::sync_directory(&installation)?;
        }
        for name in [ACTIVE, LAYOUT, LAUNCHER_STAMP] {
            let path = state.join(name);
            if path.exists() {
                storage::plain_path(&path)?;
                fs::remove_file(path)?;
            }
        }
        storage::sync_directory(&state)?;
        return Ok(0);
    }
    if operation == "uninstall-check" {
        let layout: Layout = storage::read_json(&state.join(LAYOUT))?;
        if layout.schema != SCHEMA
            || layout.product_id != PRODUCT_ID
            || layout.installation != installation
        {
            return Err(refused("Uninstall layout identity mismatch"));
        }
        validate_files(&layout.files)?;
        for file in &layout.files {
            if FileIdentity::read(&storage::owned_join(&installation, &file.path)?)?
                != file.identity()
            {
                return Err(refused(
                    "An installed payload changed; preserve it before uninstall",
                ));
            }
        }
        if let Some(txn) = active(&state, &installation)?
            && !matches!(txn.journal.phase, Phase::Committed | Phase::RolledBack)
        {
            return Err(repair("Pending native transaction blocks uninstall"));
        }
        return Ok(0);
    }
    if operation == "reconcile" {
        let plan = PathBuf::from(
            options
                .get("--plan")
                .ok_or_else(|| refused("Missing embedded plan"))?,
        );
        let plan: InstallPlan = storage::read_json(&plan)?;
        plan.validate()?;
        if plan
            .files
            .iter()
            .find(|f| f.path == LAUNCHER)
            .map(PlanFile::identity)
            != Some(FileIdentity::read(&std::env::current_exe()?)?)
        {
            return Err(refused("Reconcile helper differs from embedded plan"));
        }
        reconcile_previous(&state, &installation)?;
        return Ok(0);
    }
    if operation == "prepare" {
        let plan = PathBuf::from(
            options
                .get("--plan")
                .ok_or_else(|| refused("Missing embedded plan"))?,
        );
        let plan: InstallPlan = storage::read_json(&plan)?;
        let explorer = match options.get("--explorer").and_then(|p| p.to_str()) {
            Some("1") => true,
            Some("0") => false,
            _ => return Err(refused("Missing explorer selection")),
        };
        let package = PathBuf::from(
            options
                .get("--package")
                .ok_or_else(|| refused("Missing source Setup package"))?,
        );
        let txn = Transaction::prepare(
            &state,
            &installation,
            plan,
            parent,
            &std::env::current_exe()?,
            &package,
            explorer,
        )?;
        return Ok(if txn.journal.bootstrap { 10 } else { 0 });
    }
    let mut txn = active(&state, &installation)?
        .ok_or_else(|| repair("Setup command has no active transaction"))?;
    if FileIdentity::read(&std::env::current_exe()?)? != txn.journal.helper {
        return Err(refused("Native helper identity changed"));
    }
    if txn.journal.parent != parent {
        return Err(refused("Setup transaction belongs to another process"));
    }
    match operation {
        "begin" => txn.begin()?,
        "pending-health" => txn.pending_health()?,
        "recover" => txn.rollback("Setup failed or was cancelled")?,
        _ => return Err(refused("Unknown native operation")),
    };
    Ok(0)
}

#[cfg(windows)]
mod native {
    use super::*;
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, FILETIME, WAIT_OBJECT_0},
        Storage::FileSystem::GetDiskFreeSpaceExW,
        System::Threading::{
            GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
            QueryFullProcessImageNameW, WaitForSingleObject,
        },
    };
    fn wide(path: &Path) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }
    pub fn local_app_data() -> Result<PathBuf> {
        use windows_sys::Win32::{
            System::Com::CoTaskMemFree,
            UI::Shell::{FOLDERID_LocalAppData, SHGetKnownFolderPath},
        };
        let mut text = std::ptr::null_mut();
        let status = unsafe {
            SHGetKnownFolderPath(&FOLDERID_LocalAppData, 0, std::ptr::null_mut(), &mut text)
        };
        if status < 0 || text.is_null() {
            return Err(refused("Cannot resolve current-user LocalAppData"));
        }
        let mut len = 0;
        unsafe {
            while len < 32768 && *text.add(len) != 0 {
                len += 1;
            }
        }
        if len == 32768 {
            unsafe { CoTaskMemFree(text.cast()) };
            return Err(refused("Known-folder path exceeds bound"));
        }
        let p = PathBuf::from(OsString::from_wide(unsafe {
            std::slice::from_raw_parts(text, len)
        }));
        unsafe { CoTaskMemFree(text.cast()) };
        storage::plain_path(&p)?;
        Ok(p)
    }
    pub fn process_identity(pid: u32) -> Result<ProcessIdentity> {
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                pid,
            )
        };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        let result = (|| {
            let mut created: FILETIME = unsafe { std::mem::zeroed() };
            let mut exit = created;
            let mut kernel = created;
            let mut user = created;
            if unsafe { GetProcessTimes(handle, &mut created, &mut exit, &mut kernel, &mut user) }
                == 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
            let mut buffer = vec![0u16; 32768];
            let mut len = buffer.len() as u32;
            if unsafe { QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut len) } == 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
            let image = PathBuf::from(OsString::from_wide(&buffer[..len as usize]));
            storage::plain_path(&image)?;
            Ok(ProcessIdentity {
                pid,
                creation: ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64,
                image,
            })
        })();
        unsafe { CloseHandle(handle) };
        result
    }
    pub fn parent_pid() -> Result<u32> {
        use windows_sys::Win32::{
            Foundation::INVALID_HANDLE_VALUE,
            System::Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
                TH32CS_SNAPPROCESS,
            },
        };
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error().into());
        }
        let result = (|| {
            let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut found = unsafe { Process32FirstW(snapshot, &mut entry) };
            while found != 0 {
                if entry.th32ProcessID == std::process::id() {
                    return Ok(entry.th32ParentProcessID);
                }
                found = unsafe { Process32NextW(snapshot, &mut entry) };
            }
            Err(refused("Cannot verify actual parent process"))
        })();
        unsafe { CloseHandle(snapshot) };
        result
    }
    fn ready_event(transaction: &str) -> Vec<u16> {
        format!("Local\\GloamereRecoveryReady-{transaction}")
            .encode_utf16()
            .chain(Some(0))
            .collect()
    }
    pub fn spawn_recovery(
        helper: &Path,
        installation: &Path,
        parent: &ProcessIdentity,
        transaction: &str,
    ) -> Result<()> {
        use windows_sys::Win32::System::Threading::CreateEventW;
        let event =
            unsafe { CreateEventW(std::ptr::null(), 1, 0, ready_event(transaction).as_ptr()) };
        if event.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        let result = (|| {
            let mut child = std::process::Command::new(helper)
                .arg("--installer-transaction")
                .arg("recover-detached")
                .arg("--install-dir")
                .arg(installation)
                .arg("--parent-pid")
                .arg(parent.pid.to_string())
                .spawn()?;
            if unsafe { WaitForSingleObject(event, 30000) } != WAIT_OBJECT_0 {
                let outcome = child.try_wait()?;
                return Err(repair(format!(
                    "Recovery helper did not confirm holding parent handle ({outcome:?}); no process was killed"
                )));
            }
            Ok(())
        })();
        unsafe { CloseHandle(event) };
        result
    }
    pub fn wait_for_process(expected: &ProcessIdentity, transaction: &str) -> Result<()> {
        use windows_sys::Win32::System::Threading::{EVENT_MODIFY_STATE, OpenEventW, SetEvent};
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                expected.pid,
            )
        };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        let result = (|| {
            if process_identity(expected.pid)? != *expected {
                return Err(refused("Parent PID was reused"));
            }
            let event =
                unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, ready_event(transaction).as_ptr()) };
            if event.is_null() {
                return Err(std::io::Error::last_os_error().into());
            }
            let signalled = unsafe { SetEvent(event) };
            unsafe { CloseHandle(event) };
            if signalled == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            // Signal only after we hold this exact parent's handle. The parent
            // does not exit before the signal, so scheduling cannot lose it.
            if unsafe { WaitForSingleObject(handle, 120000) } != WAIT_OBJECT_0 {
                return Err(repair(
                    "Parent did not exit; recovery deferred without killing it",
                ));
            }
            Ok(())
        })();
        unsafe { CloseHandle(handle) };
        result
    }
    pub fn check_space(path: &Path, required: u64) -> Result<()> {
        let mut parent = path;
        while !parent.exists() {
            parent = parent
                .parent()
                .ok_or_else(|| refused("No existing volume ancestor"))?;
        }
        let mut free = 0u64;
        if unsafe {
            GetDiskFreeSpaceExW(
                wide(parent).as_ptr(),
                &mut free,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        if free < required {
            return Err(UpdateError::Deferred(
                "Insufficient free space for payload and verified rollback snapshot".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        _temp: tempfile::TempDir,
        installation: PathBuf,
        state: PathBuf,
        helper: PathBuf,
        package: PathBuf,
        plan: InstallPlan,
        parent: ProcessIdentity,
    }
    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path();
            let installation = root.join("中文 app");
            let state = root.join("state");
            fs::create_dir(&installation).unwrap();
            fs::create_dir(&state).unwrap();
            fs::write(installation.join(MARKER), PRODUCT_ID).unwrap();
            fs::write(installation.join(LAUNCHER), b"old v06 UI").unwrap();
            fs::write(
                installation.join("gloamere-image-worker.exe"),
                b"old worker",
            )
            .unwrap();
            fs::write(
                installation.join("foreign-photo.txt"),
                b"user owned, keep exact",
            )
            .unwrap();
            let old: Vec<_> = [LAUNCHER, "gloamere-image-worker.exe"]
                .iter()
                .map(|name| {
                    let id = FileIdentity::read(&installation.join(name)).unwrap();
                    serde_json::json!({"path":name,"size_bytes":id.length,"sha256":id.sha256})
                })
                .collect();
            storage::atomic_json(
                &installation.join("MANIFEST.json"),
                &serde_json::json!({"product":"gloamere-image","version":"0.6.0","files":old}),
            )
            .unwrap();
            let helper = root.join("helper.exe");
            let package = root.join("Setup.exe");
            fs::write(&helper, b"new stable launcher").unwrap();
            fs::write(&package, b"package bytes").unwrap();
            let files = [
                (LAUNCHER, b"new stable launcher".as_slice()),
                ("runtime/gloamere-image-ui.exe", b"new UI".as_slice()),
                (
                    "runtime/gloamere-image-worker.exe",
                    b"new worker".as_slice(),
                ),
                (MARKER, PRODUCT_ID.as_bytes()),
                ("MANIFEST.json", b"new manifest".as_slice()),
            ]
            .into_iter()
            .map(|(path, bytes)| {
                let p = root.join("identity");
                fs::write(&p, bytes).unwrap();
                let id = FileIdentity::read(&p).unwrap();
                PlanFile {
                    path: path.into(),
                    length: id.length,
                    sha256: id.sha256,
                }
            })
            .collect();
            let plan = InstallPlan {
                schema: 1,
                product_id: PRODUCT_ID.into(),
                version: env!("CARGO_PKG_VERSION").into(),
                launcher_protocol: 1,
                files,
            };
            let parent = ProcessIdentity {
                pid: 1234,
                creation: 9876,
                image: package.clone(),
            };
            Self {
                _temp: temp,
                installation,
                state,
                helper,
                package,
                plan,
                parent,
            }
        }
        fn prepare(&self) -> Transaction {
            Transaction::prepare(
                &self.state,
                &self.installation,
                self.plan.clone(),
                self.parent.clone(),
                &self.helper,
                &self.package,
                false,
            )
            .unwrap()
        }
        fn installed(&self) {
            for (path, bytes) in [
                ("runtime/gloamere-image-ui.exe", b"new UI".as_slice()),
                (
                    "runtime/gloamere-image-worker.exe",
                    b"new worker".as_slice(),
                ),
                ("MANIFEST.json", b"new manifest".as_slice()),
            ] {
                let p = self.installation.join(path);
                fs::create_dir_all(p.parent().unwrap()).unwrap();
                fs::write(p, bytes).unwrap();
            }
        }
        fn old_intact(&self) {
            assert_eq!(
                fs::read(self.installation.join(LAUNCHER)).unwrap(),
                b"old v06 UI"
            );
            assert_eq!(
                fs::read(self.installation.join("gloamere-image-worker.exe")).unwrap(),
                b"old worker"
            );
            assert_eq!(
                fs::read(self.installation.join("foreign-photo.txt")).unwrap(),
                b"user owned, keep exact"
            );
        }
    }
    #[test]
    fn bootstrap_is_atomic_before_other_files_and_reopens_after_loss() {
        let f = Fixture::new();
        let mut t = f.prepare();
        f.old_intact();
        assert_eq!(t.journal.phase, Phase::Prepared);
        t.begin().unwrap();
        assert_eq!(
            fs::read(f.installation.join(LAUNCHER)).unwrap(),
            b"new stable launcher"
        );
        assert_eq!(
            fs::read(f.installation.join("gloamere-image-worker.exe")).unwrap(),
            b"old worker"
        );
        drop(t);
        let mut t = active(&f.state, &f.installation).unwrap().unwrap();
        t.rollback("power failure after root swap").unwrap();
        f.old_intact();
        assert_eq!(t.journal.phase, Phase::RolledBack);
    }
    #[test]
    fn missing_or_crash_partial_old_file_restores_verified_write_intent() {
        for missing in [true, false] {
            let f = Fixture::new();
            let mut t = f.prepare();
            t.begin().unwrap();
            if missing {
                fs::remove_file(f.installation.join(LAUNCHER)).unwrap();
            } else {
                fs::write(
                    f.installation.join(LAUNCHER),
                    b"partial interrupted product write",
                )
                .unwrap();
            }
            t.rollback("interrupted write").unwrap();
            f.old_intact();
        }
    }
    #[test]
    fn new_foreign_file_is_not_overwritten_by_rollback() {
        let f = Fixture::new();
        let mut t = f.prepare();
        t.begin().unwrap();
        let p = f.installation.join("runtime/gloamere-image-ui.exe");
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, b"foreign replacement").unwrap();
        assert!(t.rollback("failure").is_err());
        assert_eq!(fs::read(p).unwrap(), b"foreign replacement");
        assert_eq!(t.journal.phase, Phase::RepairRequired);
        assert!(t.directory.join("files/gloamere-image.exe").exists());
    }
    #[test]
    fn snapshot_tampering_prevents_begin_without_payload_change() {
        let f = Fixture::new();
        let mut t = f.prepare();
        fs::write(t.directory.join("files/gloamere-image.exe"), b"bad backup").unwrap();
        assert!(t.begin().is_err());
        f.old_intact();
        assert_eq!(t.journal.phase, Phase::Prepared);
    }
    #[test]
    fn pending_health_retains_full_snapshot_and_rollback_restores_v06() {
        let f = Fixture::new();
        let mut t = f.prepare();
        t.begin().unwrap();
        f.installed();
        t.pending_health().unwrap();
        assert_eq!(t.journal.phase, Phase::PendingHealth);
        assert!(t.directory.join("files/gloamere-image.exe").exists());
        assert!(!f.installation.join("gloamere-image-worker.exe").exists());
        drop(t);
        let mut t = active(&f.state, &f.installation).unwrap().unwrap();
        t.rollback("UI did not become healthy").unwrap();
        f.old_intact();
    }
    #[test]
    fn prepare_rejects_changed_old_and_foreign_target_collisions() {
        for old in [true, false] {
            let f = Fixture::new();
            if old {
                fs::write(f.installation.join(LAUNCHER), b"modified preexisting").unwrap();
            } else {
                let path = f.installation.join("runtime/gloamere-image-ui.exe");
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, b"foreign target").unwrap();
            }
            assert!(
                Transaction::prepare(
                    &f.state,
                    &f.installation,
                    f.plan.clone(),
                    f.parent.clone(),
                    &f.helper,
                    &f.package,
                    false
                )
                .is_err()
            );
        }
        let f = Fixture::new();
        let foreign = f.installation.join("unins999.exe");
        fs::write(&foreign, b"foreign product with uninstaller-like name").unwrap();
        assert!(
            Transaction::prepare(
                &f.state,
                &f.installation,
                f.plan.clone(),
                f.parent.clone(),
                &f.helper,
                &f.package,
                false
            )
            .is_err()
        );
        assert_eq!(
            fs::read(foreign).unwrap(),
            b"foreign product with uninstaller-like name"
        );
        f.old_intact();
    }
    #[test]
    fn committed_layout_keeps_stable_launcher_on_next_release() {
        let f = Fixture::new();
        let mut t = f.prepare();
        t.begin().unwrap();
        f.installed();
        t.pending_health().unwrap();
        t.journal.phase = Phase::CommittingHealth;
        t.persist().unwrap();
        t.finish_commit(&f.state).unwrap();
        let original = FileIdentity::read(&f.installation.join(LAUNCHER)).unwrap();
        assert_eq!(
            fs::read_to_string(f.state.join(LAUNCHER_STAMP)).unwrap(),
            format!("{PRODUCT_ID}\n{}\n", original.sha256)
        );
        let mut next = f.plan.clone();
        fs::write(&f.helper, b"different future stable launcher").unwrap();
        let id = FileIdentity::read(&f.helper).unwrap();
        let root = next.files.iter_mut().find(|p| p.path == LAUNCHER).unwrap();
        root.length = id.length;
        root.sha256 = id.sha256;
        let mut next = Transaction::prepare(
            &f.state,
            &f.installation,
            next,
            f.parent.clone(),
            &f.helper,
            &f.package,
            false,
        )
        .unwrap();
        assert!(!next.journal.bootstrap);
        next.begin().unwrap();
        assert_eq!(
            FileIdentity::read(&f.installation.join(LAUNCHER)).unwrap(),
            original
        );
    }
    #[test]
    fn invalid_plan_cannot_touch_settings_or_escape() {
        let f = Fixture::new();
        for name in [
            "../settings.json",
            "settings.json",
            "runtime/../x",
            "unins000.exe",
            "C:/Windows/file",
            "other-user.txt",
        ] {
            let mut plan = f.plan.clone();
            plan.files.push(PlanFile {
                path: name.into(),
                length: 1,
                sha256: "a".repeat(64),
            });
            assert!(plan.validate().is_err(), "{name}");
        }
        f.old_intact();
    }
    #[test]
    fn cancel_before_begin_is_nonmutating_and_journal_is_retained() {
        let f = Fixture::new();
        let mut t = f.prepare();
        t.rollback("cancel before commit").unwrap();
        f.old_intact();
        assert_eq!(t.journal.phase, Phase::RolledBack);
        assert!(t.directory.join("journal.json").exists());
        assert!(t.directory.join("files/gloamere-image.exe").exists());
    }
    #[cfg(unix)]
    #[test]
    fn reparse_ancestor_and_changed_helper_are_rejected() {
        let f = Fixture::new();
        let alternate = f._temp.path().join("alias");
        std::os::unix::fs::symlink(&f.installation, &alternate).unwrap();
        assert!(
            Transaction::prepare(
                &f.state,
                &alternate,
                f.plan.clone(),
                f.parent.clone(),
                &f.helper,
                &f.package,
                false
            )
            .is_err()
        );
        fs::write(&f.helper, b"untrusted helper").unwrap();
        assert!(
            Transaction::prepare(
                &f.state,
                &f.installation,
                f.plan.clone(),
                f.parent.clone(),
                &f.helper,
                &f.package,
                false
            )
            .is_err()
        );
    }
    #[test]
    fn injected_disk_full_or_power_interruption_recovers_or_leaves_original_untouched() {
        for checkpoint in 0..18 {
            let f = Fixture::new();
            FAIL_AFTER.with(|n| n.set(Some(checkpoint)));
            let result = Transaction::prepare(
                &f.state,
                &f.installation,
                f.plan.clone(),
                f.parent.clone(),
                &f.helper,
                &f.package,
                false,
            )
            .and_then(|mut t| {
                t.begin()?;
                f.installed();
                t.pending_health()
            });
            FAIL_AFTER.with(|n| n.set(None));
            if result.is_err() {
                if let Some(mut t) = active(&f.state, &f.installation).unwrap() {
                    t.rollback("injected interrupted stage").unwrap();
                }
                f.old_intact();
            }
        }
    }
    #[test]
    fn rollback_interruption_can_resume_idempotently() {
        let f = Fixture::new();
        let mut t = f.prepare();
        t.begin().unwrap();
        f.installed();
        t.pending_health().unwrap();
        FAIL_AFTER.with(|n| n.set(Some(1)));
        assert!(t.rollback("test interrupted rollback").is_err());
        FAIL_AFTER.with(|n| n.set(None));
        let mut t = active(&f.state, &f.installation).unwrap().unwrap();
        t.rollback("resume").unwrap();
        f.old_intact();
        assert_eq!(t.journal.phase, Phase::RolledBack);
    }
    #[test]
    fn bootstrap_root_is_restored_last_across_interruption() {
        let f = Fixture::new();
        let mut txn = f.prepare();
        txn.begin().unwrap();
        f.installed();
        txn.pending_health().unwrap();
        let count = txn
            .journal
            .files
            .iter()
            .filter(|f| f.path != LAUNCHER)
            .count();
        FAIL_AFTER.with(|n| n.set(Some(count)));
        assert!(txn.rollback("powerloss before final root swap").is_err());
        FAIL_AFTER.with(|n| n.set(None));
        assert_eq!(
            fs::read(f.installation.join(LAUNCHER)).unwrap(),
            b"new stable launcher"
        );
        assert_eq!(
            fs::read(f.installation.join("gloamere-image-worker.exe")).unwrap(),
            b"old worker"
        );
        let mut txn = active(&f.state, &f.installation).unwrap().unwrap();
        txn.rollback("resume final root swap").unwrap();
        f.old_intact();
    }
    #[test]
    fn rerunning_setup_recovers_gap_before_atomic_bootstrap_swap() {
        let f = Fixture::new();
        let mut txn = f.prepare();
        txn.journal.integration.mark_write_intent().unwrap();
        txn.journal.phase = Phase::Installing;
        txn.persist().unwrap();
        f.old_intact();
        reconcile_previous(&f.state, &f.installation).unwrap();
        f.old_intact();
        assert_eq!(
            active(&f.state, &f.installation)
                .unwrap()
                .unwrap()
                .journal
                .phase,
            Phase::RolledBack
        );
        assert_eq!(f.prepare().journal.phase, Phase::Prepared);
    }
    #[test]
    fn fresh_absent_install_directory_is_supported_without_preexisting_marker() {
        let f = Fixture::new();
        let root = f._temp.path().join("fresh install");
        let mut txn = Transaction::prepare(
            &f.state,
            &root,
            f.plan.clone(),
            f.parent.clone(),
            &f.helper,
            &f.package,
            false,
        )
        .unwrap();
        assert!(txn.journal.bootstrap);
        assert!(txn.journal.files.iter().all(|f| f.before.is_none()));
        txn.begin().unwrap();
        assert_eq!(
            fs::read(root.join(LAUNCHER)).unwrap(),
            b"new stable launcher"
        );
    }
    #[test]
    fn runtime_admission_blocks_incomplete_states_and_keeps_healthy_workers_available() {
        let f = Fixture::new();
        let mut txn = f.prepare();
        let ticket = txn.journal.transaction.clone();
        for phase in [
            Phase::Preparing,
            Phase::Prepared,
            Phase::Installing,
            Phase::RollingBack,
            Phase::RepairRequired,
        ] {
            txn.journal.phase = phase;
            assert!(runtime_state_admission(&txn.journal, None).is_err());
            assert!(runtime_state_admission(&txn.journal, Some(&ticket)).is_err());
        }
        txn.journal.phase = Phase::PendingHealth;
        assert!(runtime_state_admission(&txn.journal, Some(&ticket)).is_err());
        txn.journal.health_attempted = true;
        assert!(runtime_state_admission(&txn.journal, Some(&ticket)).is_ok());
        assert!(runtime_state_admission(&txn.journal, None).is_err());
        txn.journal.phase = Phase::Committed;
        assert!(runtime_state_admission(&txn.journal, Some(&ticket)).is_ok());
        assert!(runtime_state_admission(&txn.journal, None).is_ok());
        assert!(runtime_state_admission(&txn.journal, Some("foreign")).is_err());
    }
    #[test]
    fn disk_budget_combines_backup_expansion_restore_temp_and_helper() {
        assert_eq!(space_budget(10, 20, 7, 3).unwrap(), 64 * 1024 * 1024 + 40);
        assert!(space_budget(u64::MAX, 1, 0, 0).is_err());
    }
    fn retention_fixture() -> (Fixture, Vec<Transaction>) {
        let f = Fixture::new();
        let mut transactions = Vec::new();
        for _ in 0..3 {
            let mut t = f.prepare();
            t.begin().unwrap();
            f.installed();
            t.pending_health().unwrap();
            // Build three healthy historical sets without running cleanup yet.
            let link = t.journal.retention_link.take();
            t.finish_commit(&f.state).unwrap();
            t.journal.retention_link = link;
            t.persist().unwrap();
            t._directory_pins.clear();
            transactions.push(t);
        }
        (f, transactions)
    }
    #[test]
    fn retention_keeps_active_and_previous_exact_terminal_sets() {
        let (f, transactions) = retention_fixture();
        let current = &transactions[2].journal;
        retention::run(&f.state, current).unwrap();
        assert!(!transactions[0].directory.exists());
        for t in &transactions[1..] {
            t.verify_backups().unwrap();
        }
        assert_eq!(
            active(&f.state, &f.installation)
                .unwrap()
                .unwrap()
                .journal
                .phase,
            Phase::Committed
        );
        assert!(f.installation.join("foreign-photo.txt").exists());
        assert!(
            f.state
                .join("installer-retention")
                .join(format!("{}.json", transactions[0].journal.transaction))
                .exists()
        );
        retention::run(&f.state, current).unwrap();
    }
    #[test]
    fn retention_crash_before_and_after_each_delete_resumes_without_rollback() {
        let mut failures = 0;
        for cutoff in 0..40 {
            let (f, transactions) = retention_fixture();
            FAIL_AFTER.with(|v| v.set(Some(cutoff)));
            if retention::run(&f.state, &transactions[2].journal).is_err() {
                failures += 1;
            }
            FAIL_AFTER.with(|v| v.set(None));
            assert_eq!(
                active(&f.state, &f.installation)
                    .unwrap()
                    .unwrap()
                    .journal
                    .phase,
                Phase::Committed
            );
            transactions[1].verify_backups().unwrap();
            transactions[2].verify_target().unwrap();
            retention::run(&f.state, &transactions[2].journal).unwrap();
            assert!(!transactions[0].directory.exists());
        }
        assert!(
            failures >= 15,
            "Fault matrix must cross file, directory and finalization boundaries"
        );
    }
    #[test]
    fn retention_foreign_changed_and_nonterminal_sets_are_preserved() {
        for variant in 0..4 {
            let (f, mut transactions) = retention_fixture();
            let old = &mut transactions[0];
            match variant {
                0 => fs::write(old.directory.join("foreign.txt"), b"not ours").unwrap(),
                1 => fs::write(
                    old.directory.join("files").join(LAUNCHER),
                    b"changed backup",
                )
                .unwrap(),
                2 => {
                    old.journal.phase = Phase::RepairRequired;
                    old.persist().unwrap();
                }
                _ => {
                    old.journal.installation = f._temp.path().join("other-installation");
                    old.journal.integration = integration::Snapshot::capture(
                        &old.journal.installation,
                        &old.directory,
                        false,
                        &old.journal.version,
                    )
                    .unwrap();
                    old.persist().unwrap();
                }
            }
            retention::best_effort(&f.state, &transactions[2].journal);
            assert!(transactions[0].directory.join("journal.json").exists());
            assert_eq!(
                active(&f.state, &f.installation)
                    .unwrap()
                    .unwrap()
                    .journal
                    .phase,
                Phase::Committed
            );
            transactions[2].verify_target().unwrap();
        }
    }
    #[test]
    fn retention_requires_exact_prior_intent_for_missing_entries() {
        let (f, transactions) = retention_fixture();
        FAIL_AFTER.with(|v| v.set(Some(0))); // Receipt durable, no deletion authorized.
        assert!(retention::run(&f.state, &transactions[2].journal).is_err());
        FAIL_AFTER.with(|v| v.set(None));
        fs::remove_file(transactions[0].directory.join("files").join(LAUNCHER)).unwrap();
        assert!(retention::run(&f.state, &transactions[2].journal).is_err());
        assert!(transactions[0].directory.join("journal.json").exists());
    }
    #[test]
    fn retention_rechecks_newly_protected_predecessor_and_stale_current() {
        let (f, mut transactions) = retention_fixture();
        FAIL_AFTER.with(|v| v.set(Some(0)));
        assert!(retention::run(&f.state, &transactions[2].journal).is_err());
        FAIL_AFTER.with(|v| v.set(None));
        let stale = transactions[2].journal.clone();
        transactions[2]
            .journal
            .retention_link
            .as_mut()
            .unwrap()
            .previous_terminal = Some(transactions[0].journal.transaction.clone());
        transactions[2].persist().unwrap();
        assert!(retention::run(&f.state, &stale).is_err());
        retention::run(&f.state, &transactions[2].journal).unwrap();
        transactions[0].verify_backups().unwrap();
    }
    #[test]
    fn retention_legacy_journal_without_predecessor_link_is_untouched() {
        let (f, mut transactions) = retention_fixture();
        transactions[2].journal.retention_link = None;
        transactions[2].persist().unwrap();
        retention::run(&f.state, &transactions[2].journal).unwrap();
        transactions[0].verify_backups().unwrap();
    }
    #[test]
    fn retention_maintenance_serializes_admission_without_excluding_healthy_sessions() {
        let (f, transactions) = retention_fixture();
        let _maintenance =
            storage::LockGuard::acquire(&f.state.join("maintenance.lock"), false).unwrap();
        let _healthy_session =
            storage::LockGuard::acquire(&f.state.join("sessions.lock"), true).unwrap();
        assert!(storage::LockGuard::acquire(&f.state.join("maintenance.lock"), false).is_err());
        retention::run(&f.state, &transactions[2].journal).unwrap();
        runtime_state_admission(&transactions[2].journal, None).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn retention_links_and_permissions_conflicts_preserve_evidence() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        for link in [false, true] {
            let (f, transactions) = retention_fixture();
            FAIL_AFTER.with(|v| v.set(Some(0)));
            assert!(retention::run(&f.state, &transactions[2].journal).is_err());
            FAIL_AFTER.with(|v| v.set(None));
            let path = transactions[0].directory.join("files").join(LAUNCHER);
            if link {
                fs::remove_file(&path).unwrap();
                symlink(f.installation.join(LAUNCHER), &path).unwrap();
            } else {
                let mode = fs::metadata(&path).unwrap().permissions().mode() ^ 0o040;
                fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            }
            assert!(retention::run(&f.state, &transactions[2].journal).is_err());
            assert!(transactions[0].directory.join("journal.json").exists());
            transactions[2].verify_target().unwrap();
        }
    }
    #[test]
    fn retention_initial_missing_backup_is_not_adopted_as_retired() {
        let (f, transactions) = retention_fixture();
        fs::remove_file(transactions[0].directory.join("files").join(LAUNCHER)).unwrap();
        assert!(retention::run(&f.state, &transactions[2].journal).is_err());
        assert!(transactions[0].directory.join("journal.json").exists());
    }
    #[test]
    fn retention_empty_files_directory_is_explicitly_owned_and_retired() {
        let (f, mut transactions) = retention_fixture();
        let old = &mut transactions[0];
        // Scoped synthetic clean-install snapshot has no preexisting files.
        for file in &mut old.journal.files {
            if file.before.take().is_some() {
                fs::remove_file(
                    storage::owned_join(&old.directory.join("files"), &file.path).unwrap(),
                )
                .unwrap();
            }
        }
        old.journal.retention_permissions =
            Some(retention::snapshot_permissions(&old.directory).unwrap());
        old.persist().unwrap();
        retention::run(&f.state, &transactions[2].journal).unwrap();
        assert!(!transactions[0].directory.exists());
    }
    #[cfg(unix)]
    #[test]
    fn retention_detects_permissions_changed_before_initial_intent() {
        use std::os::unix::fs::PermissionsExt;
        let (f, transactions) = retention_fixture();
        let path = transactions[0].directory.join("files").join(LAUNCHER);
        let mut mode = fs::metadata(&path).unwrap().permissions().mode();
        mode ^= 0o040;
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        assert!(retention::run(&f.state, &transactions[2].journal).is_err());
        assert!(transactions[0].directory.join("journal.json").exists());
    }
    #[test]
    fn retention_rejects_escaped_receipt_paths_before_any_deletion() {
        let (f, transactions) = retention_fixture();
        FAIL_AFTER.with(|v| v.set(Some(0)));
        assert!(retention::run(&f.state, &transactions[2].journal).is_err());
        FAIL_AFTER.with(|v| v.set(None));
        let receipt = f
            .state
            .join("installer-retention")
            .join(format!("{}.json", transactions[0].journal.transaction));
        let mut data: serde_json::Value = storage::read_json(&receipt).unwrap();
        data["files"][0]["path"] = serde_json::json!("files/../../settings.json");
        storage::atomic_json(&receipt, &data).unwrap();
        assert!(retention::run(&f.state, &transactions[2].journal).is_err());
        transactions[0].verify_backups().unwrap();
        transactions[2].verify_target().unwrap();
    }
    #[test]
    fn retention_receipt_budget_defers_without_rolling_back_health() {
        let (f, transactions) = retention_fixture();
        let receipts = f.state.join("installer-retention");
        fs::create_dir(&receipts).unwrap();
        fs::write(receipts.join("PRODUCT-OWNER.txt"), PRODUCT_ID).unwrap();
        for n in 0..128 {
            fs::write(receipts.join(format!("installer-9-{n}.json")), b"{}").unwrap();
        }
        retention::best_effort(&f.state, &transactions[2].journal);
        transactions[0].verify_backups().unwrap();
        transactions[2].verify_target().unwrap();
        assert_eq!(fs::read_dir(&receipts).unwrap().count(), 129);
        assert!(f.state.join("installer-retention-warning.json").exists());
        assert_eq!(
            active(&f.state, &f.installation)
                .unwrap()
                .unwrap()
                .journal
                .phase,
            Phase::Committed
        );
    }
    #[test]
    fn retention_after_failed_release_keeps_previous_healthy_preimages() {
        let f = Fixture::new();
        let mut a = f.prepare();
        a.begin().unwrap();
        f.installed();
        a.pending_health().unwrap();
        a.finish_commit(&f.state).unwrap();
        a._directory_pins.clear();
        let healthy_a =
            FileIdentity::read(&f.installation.join("runtime/gloamere-image-ui.exe")).unwrap();
        let plan_with_ui = |bytes: &[u8]| {
            let path = f._temp.path().join("synthetic-ui-identity");
            fs::write(&path, bytes).unwrap();
            let id = FileIdentity::read(&path).unwrap();
            let mut plan = f.plan.clone();
            let file = plan
                .files
                .iter_mut()
                .find(|p| p.path == "runtime/gloamere-image-ui.exe")
                .unwrap();
            file.length = id.length;
            file.sha256 = id.sha256;
            plan
        };
        let mut b = Transaction::prepare(
            &f.state,
            &f.installation,
            plan_with_ui(b"failed B UI"),
            f.parent.clone(),
            &f.helper,
            &f.package,
            false,
        )
        .unwrap();
        b.begin().unwrap();
        f.installed();
        fs::write(
            f.installation.join("runtime/gloamere-image-ui.exe"),
            b"failed B UI",
        )
        .unwrap();
        b.pending_health().unwrap();
        b.rollback("B health failed").unwrap();
        b._directory_pins.clear();
        assert_eq!(
            FileIdentity::read(&f.installation.join("runtime/gloamere-image-ui.exe")).unwrap(),
            healthy_a
        );
        let mut c = Transaction::prepare(
            &f.state,
            &f.installation,
            plan_with_ui(b"healthy C UI"),
            f.parent.clone(),
            &f.helper,
            &f.package,
            false,
        )
        .unwrap();
        c.begin().unwrap();
        f.installed();
        fs::write(
            f.installation.join("runtime/gloamere-image-ui.exe"),
            b"healthy C UI",
        )
        .unwrap();
        c.pending_health().unwrap();
        c.finish_commit(&f.state).unwrap();
        assert_eq!(
            c.journal
                .retention_link
                .as_ref()
                .unwrap()
                .previous_terminal
                .as_deref(),
            Some(b.journal.transaction.as_str())
        );
        assert!(!a.directory.exists());
        assert!(b.directory.exists());
        c.verify_backups().unwrap();
        // Reconstruct the previous healthy payload in a separate scoped tree.
        // A committed healthy installation is never automatically rolled back.
        let reconstructed = f._temp.path().join("reconstructed-healthy-A");
        for file in &c.journal.files {
            if let Some(id) = &file.before {
                copy_durable(
                    &storage::owned_join(&c.directory.join("files"), &file.path).unwrap(),
                    &storage::owned_join(&reconstructed, &file.path).unwrap(),
                    id,
                    false,
                )
                .unwrap();
            }
        }
        assert_eq!(
            FileIdentity::read(&reconstructed.join("runtime/gloamere-image-ui.exe")).unwrap(),
            healthy_a
        );
        assert_eq!(c.journal.phase, Phase::Committed);
        c.verify_target().unwrap();
    }

    #[test]
    fn active_pointer_rejects_mismatched_journal_root_before_recovery() {
        let f = Fixture::new();
        let txn = f.prepare();
        let other = f._temp.path().join("different installation");
        fs::create_dir(&other).unwrap();
        let mut journal = serde_json::to_value(&txn.journal).unwrap();
        journal["installation"] = serde_json::json!(other);
        journal["integration"]["installation"] = serde_json::json!(other);
        storage::atomic_json(&txn.directory.join("journal.json"), &journal).unwrap();
        let before = fs::read(txn.directory.join("journal.json")).unwrap();
        assert!(
            matches!(active(&f.state, &f.installation), Err(UpdateError::Repair(message))
            if message.contains("pointer/journal installation mismatch"))
        );
        assert_eq!(
            fs::read(txn.directory.join("journal.json")).unwrap(),
            before
        );
        assert_eq!(fs::read_dir(&other).unwrap().count(), 0);
        f.old_intact();
    }
    #[test]
    fn preparing_and_terminal_pointer_still_reject_different_requested_root() {
        let f = Fixture::new();
        let mut txn = f.prepare();
        let other = f._temp.path().join("different requested root");
        for phase in [Phase::Preparing, Phase::RolledBack] {
            txn.journal.phase = phase;
            txn.persist().unwrap();
            assert!(
                matches!(active(&f.state, &other), Err(UpdateError::Repair(message))
                if message.contains("identity/path mismatch"))
            );
            assert_eq!(
                active(&f.state, &f.installation)
                    .unwrap()
                    .unwrap()
                    .journal
                    .phase,
                phase
            );
            f.old_intact();
        }
    }
    #[test]
    fn initial_reboot_refusal_publishes_no_preparing_pointer_or_snapshot() {
        for error in [false, true] {
            let f = Fixture::new();
            let before: BTreeSet<_> = fs::read_dir(&f.state)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            let mut calls = 0;
            let result = Transaction::prepare_with_reboot_probe(
                &f.state,
                &f.installation,
                f.plan.clone(),
                f.parent.clone(),
                &f.helper,
                &f.package,
                false,
                || {
                    calls += 1;
                    if error {
                        Err(refused("injected oversized or unavailable reboot value"))
                    } else {
                        Ok(false)
                    }
                },
            );
            assert!(matches!(result, Err(UpdateError::Deferred(_))));
            assert_eq!(calls, 1);
            assert!(!f.state.join(ACTIVE).exists());
            let after: BTreeSet<_> = fs::read_dir(&f.state)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            assert_eq!(before, after);
            f.old_intact();
        }
    }
    #[test]
    fn reboot_change_after_snapshot_remains_recoverable_without_payload_mutation() {
        for error in [false, true] {
            let f = Fixture::new();
            let mut calls = 0;
            let result = Transaction::prepare_with_reboot_probe(
                &f.state,
                &f.installation,
                f.plan.clone(),
                f.parent.clone(),
                &f.helper,
                &f.package,
                false,
                || {
                    calls += 1;
                    if calls == 1 {
                        Ok(true)
                    } else if error {
                        Err(refused("injected late reboot read error"))
                    } else {
                        Ok(false)
                    }
                },
            );
            assert!(result.is_err());
            assert_eq!(calls, 2);
            assert_eq!(
                active(&f.state, &f.installation)
                    .unwrap()
                    .unwrap()
                    .journal
                    .phase,
                Phase::Preparing
            );
            f.old_intact();
            reconcile_previous(&f.state, &f.installation).unwrap();
            assert_eq!(
                active(&f.state, &f.installation)
                    .unwrap()
                    .unwrap()
                    .journal
                    .phase,
                Phase::RolledBack
            );
            f.old_intact();
        }
    }

    #[test]
    fn initial_reboot_refusal_preserves_existing_terminal_transaction() {
        for error in [false, true] {
            let f = Fixture::new();
            let mut previous = f.prepare();
            previous.rollback("cancelled before mutation").unwrap();
            previous._directory_pins.clear();
            let pointer = fs::read(f.state.join(ACTIVE)).unwrap();
            let journal = fs::read(previous.directory.join("journal.json")).unwrap();
            let directories: BTreeSet<_> = fs::read_dir(&f.state)
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            let result = Transaction::prepare_with_reboot_probe(
                &f.state,
                &f.installation,
                f.plan.clone(),
                f.parent.clone(),
                &f.helper,
                &f.package,
                false,
                || {
                    if error {
                        Err(refused("injected initial reboot error"))
                    } else {
                        Ok(false)
                    }
                },
            );
            assert!(matches!(result, Err(UpdateError::Deferred(_))));
            assert_eq!(fs::read(f.state.join(ACTIVE)).unwrap(), pointer);
            assert_eq!(
                fs::read(previous.directory.join("journal.json")).unwrap(),
                journal
            );
            assert_eq!(
                fs::read_dir(&f.state)
                    .unwrap()
                    .map(|e| e.unwrap().file_name())
                    .collect::<BTreeSet<_>>(),
                directories
            );
            previous.verify_backups().unwrap();
            f.old_intact();
        }
    }

    #[test]
    fn initial_reboot_refusal_preserves_reconciled_preparing_evidence() {
        for error in [false, true] {
            let f = Fixture::new();
            let mut previous = f.prepare();
            previous.journal.phase = Phase::Preparing;
            previous.persist().unwrap();
            previous._directory_pins.clear();
            let pointer = fs::read(f.state.join(ACTIVE)).unwrap();
            let directories: BTreeSet<_> = fs::read_dir(&f.state)
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            let result = Transaction::prepare_with_reboot_probe(
                &f.state,
                &f.installation,
                f.plan.clone(),
                f.parent.clone(),
                &f.helper,
                &f.package,
                false,
                || {
                    if error {
                        Err(refused("injected initial reboot error"))
                    } else {
                        Ok(false)
                    }
                },
            );
            assert!(matches!(result, Err(UpdateError::Deferred(_))));
            assert_eq!(fs::read(f.state.join(ACTIVE)).unwrap(), pointer);
            let recovered = active(&f.state, &f.installation).unwrap().unwrap();
            assert_eq!(recovered.journal.phase, Phase::RolledBack);
            assert_eq!(recovered.journal.transaction, previous.journal.transaction);
            recovered.verify_backups().unwrap();
            assert_eq!(
                fs::read_dir(&f.state)
                    .unwrap()
                    .map(|e| e.unwrap().file_name())
                    .collect::<BTreeSet<_>>(),
                directories
            );
            f.old_intact();
        }
    }

    #[test]
    fn delegated_lock_evidence_rejects_security_and_io_errors() {
        assert!(require_delegated_lock(Err(UpdateError::Deferred("contention".into()))).is_ok());
        for error in [
            UpdateError::PathOwnerMismatch,
            UpdateError::PathUnsafeWriteAccess,
            refused("reparse"),
            UpdateError::Io(std::io::ErrorKind::PermissionDenied.into()),
        ] {
            assert!(require_delegated_lock(Err(error)).is_err());
        }
        let temp = tempfile::tempdir().unwrap();
        let unlocked = storage::LockGuard::acquire(&temp.path().join("unheld.lock"), false);
        assert!(require_delegated_lock(unlocked).is_err());
    }

    #[test]
    fn security_diagnostic_exit_codes_are_fixed_and_do_not_parse_error_text() {
        assert_eq!(
            security_diagnostic_exit(&UpdateError::PathOwnerMismatch),
            Some(41)
        );
        assert_eq!(
            security_diagnostic_exit(&UpdateError::PathUnsafeWriteAccess),
            Some(42)
        );
        assert_eq!(
            security_diagnostic_exit(&refused("PathOwnerMismatch 41")),
            None
        );
        assert_eq!(security_diagnostic_exit(&repair("42")), None);
        assert_eq!(
            security_diagnostic_exit(&std::io::Error::from_raw_os_error(41).into()),
            None
        );
    }
}
