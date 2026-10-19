//! Post-health retirement of old recovery bytes. Never participates in rollback.
use super::*;

const DIRECTORY: &str = "installer-retention";
const OWNER: &str = "PRODUCT-OWNER.txt";
const MAX_SETS: usize = 1024;
const MAX_ENTRIES: usize = MAX_FILES * 4 + 16;
const MAX_RECEIPTS: usize = 128;
const MAX_RECEIPT_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Link {
    pub previous_terminal: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetiredFile {
    path: String,
    identity: FileIdentity,
    permissions: platform::FilePermissions,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetiredDirectory {
    path: String,
    permissions: platform::FilePermissions,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    schema: u32,
    product_id: String,
    transaction: String,
    installation: PathBuf,
    files: Vec<RetiredFile>,
    directories: Vec<RetiredDirectory>,
    finalizing: bool,
    deleting: Option<String>,
    completed: bool,
    authorized_deletions: BTreeSet<String>,
}
fn name_valid(name: &str) -> bool {
    let Some(parts) = name.strip_prefix("installer-") else {
        return false;
    };
    let mut parts = parts.split('-');
    name.len() <= 96
        && parts
            .next()
            .is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        && parts
            .next()
            .is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        && parts.next().is_none()
}

fn same_permissions(a: &platform::FilePermissions, b: &platform::FilePermissions) -> bool {
    a.readonly == b.readonly && a.unix_mode == b.unix_mode && a.windows_sddl == b.windows_sddl
}
fn checked_permissions(path: &Path) -> Result<platform::FilePermissions> {
    platform::verify_current_user_path(path)?;
    platform::capture_permissions(path)
}
fn ancestors(path: &str, target: &mut BTreeSet<String>) {
    let mut parent = Path::new(path).parent();
    while let Some(p) = parent {
        let value = p.to_string_lossy().replace('\\', "/");
        target.insert(value);
        parent = p.parent();
    }
    target.insert(String::new());
}
fn paths(directory: &Path) -> Result<(BTreeSet<String>, BTreeSet<String>)> {
    let mut files = BTreeSet::new();
    let mut dirs = BTreeSet::new();
    let mut pending = vec![(directory.to_path_buf(), String::new())];
    while let Some((path, relative)) = pending.pop() {
        storage::plain_path(&path)?;
        checked_permissions(&path)?;
        if !fs::metadata(&path)?.is_dir() {
            return Err(refused("Retention directory is not a directory"));
        }
        dirs.insert(relative.clone());
        let _pins = platform::pin_directories(&path)?;
        for entry in fs::read_dir(&path)? {
            let entry = entry?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| refused("Non-UTF8 retention entry"))?;
            let name = if relative.is_empty() {
                name
            } else {
                format!("{relative}/{name}")
            };
            let child = storage::owned_join(directory, &name)?;
            let metadata = fs::symlink_metadata(&child)?;
            if metadata.is_dir() {
                pending.push((child, name));
            } else if metadata.is_file() {
                files.insert(name);
            } else {
                return Err(refused("Non-regular retention entry"));
            }
            if files.len() + dirs.len() + pending.len() > MAX_ENTRIES {
                return Err(refused("Retention inventory exceeds bound"));
            }
        }
    }
    Ok((files, dirs))
}
pub(super) fn snapshot_permissions(
    directory: &Path,
) -> Result<BTreeMap<String, platform::FilePermissions>> {
    let (files, directories) = paths(directory)?;
    let mut result = BTreeMap::new();
    for file in files {
        result.insert(
            file.clone(),
            checked_permissions(&storage::owned_join(directory, &file)?)?,
        );
    }
    for relative in directories {
        let path = if relative.is_empty() {
            directory.to_path_buf()
        } else {
            storage::owned_join(directory, &relative)?
        };
        result.insert(format!("directory:{relative}"), checked_permissions(&path)?);
    }
    Ok(result)
}
fn capture(state: &Path, installation: &Path, name: &str) -> Result<Option<Intent>> {
    if !name_valid(name) {
        return Err(refused("Invalid retention transaction"));
    }
    let directory = state.join(name);
    storage::plain_path(&directory)?;
    if !directory.is_dir() {
        return Ok(None);
    }
    let _pins = platform::pin_directories(&directory)?;
    checked_permissions(&directory)?;
    if fs::read_to_string(directory.join(OWNER))? != PRODUCT_ID {
        return Err(refused("Retention owner differs"));
    }
    let journal: Journal = storage::read_json(&directory.join("journal.json"))?;
    if journal.installation != installation
        || !matches!(journal.phase, Phase::Committed | Phase::RolledBack)
    {
        return Ok(None);
    }
    let txn = Transaction {
        directory: directory.clone(),
        journal,
        _directory_pins: Vec::new(),
    };
    txn.validate()?;
    let Some(baseline) = &txn.journal.retention_permissions else {
        return Ok(None);
    };
    let mut expected = BTreeMap::new();
    for file in &txn.journal.files {
        if let Some(id) = &file.before {
            expected.insert(format!("files/{}", file.path), id.clone());
        }
    }
    expected.insert("recovery-helper.exe".into(), txn.journal.helper.clone());
    for name in [OWNER, "journal.json"] {
        expected.insert(name.into(), FileIdentity::read(&directory.join(name))?);
    }
    let parent_path = directory.join("recovery-parent.json");
    if parent_path.exists() {
        let identity: ProcessIdentity = storage::read_json(&parent_path)?;
        if identity.pid == 0
            || identity.creation == 0
            || identity.image != installation.join(LAUNCHER)
        {
            return Err(refused("Retention recovery-parent identity differs"));
        }
        expected.insert(
            "recovery-parent.json".into(),
            FileIdentity::read(&parent_path)?,
        );
    }
    let mut expected_dirs = BTreeSet::new();
    for path in expected.keys() {
        ancestors(path, &mut expected_dirs);
    }
    expected_dirs.insert("files".into()); // A fixed empty snapshot container is harmless.
    let (actual_files, actual_dirs) = paths(&directory)?;
    if actual_files != expected.keys().cloned().collect() || !actual_dirs.is_subset(&expected_dirs)
    {
        return Err(refused("Unknown recovery entries; retention preserved"));
    }
    let mut files = Vec::new();
    for path in actual_files {
        let full = storage::owned_join(&directory, &path)?;
        let identity = FileIdentity::read(&full)?;
        if expected.get(&path) != Some(&identity) {
            return Err(refused("Changed recovery bytes; retention preserved"));
        }
        let permissions = checked_permissions(&full)?;
        if !baseline
            .get(&path)
            .is_some_and(|p| same_permissions(p, &permissions))
        {
            return Err(refused(
                "Snapshot file permissions changed before retention",
            ));
        }
        files.push(RetiredFile {
            path,
            identity,
            permissions,
        });
    }
    let mut directories = Vec::new();
    for path in actual_dirs {
        let full = if path.is_empty() {
            directory.clone()
        } else {
            storage::owned_join(&directory, &path)?
        };
        let permissions = checked_permissions(&full)?;
        if !baseline
            .get(&format!("directory:{path}"))
            .is_some_and(|p| same_permissions(p, &permissions))
        {
            return Err(refused(
                "Snapshot directory permissions changed before retention",
            ));
        }
        directories.push(RetiredDirectory { path, permissions });
    }
    Ok(Some(Intent {
        schema: SCHEMA,
        product_id: PRODUCT_ID.into(),
        transaction: name.into(),
        installation: installation.into(),
        files,
        directories,
        finalizing: false,
        deleting: None,
        completed: false,
        authorized_deletions: BTreeSet::new(),
    }))
}
impl Intent {
    fn validate(&self, installation: &Path, name: &str) -> Result<()> {
        if self.schema != SCHEMA
            || self.product_id != PRODUCT_ID
            || self.installation != installation
            || self.transaction != name
            || !name_valid(name)
            || self.files.len() > MAX_FILES + 4
            || self.directories.len() > MAX_ENTRIES
        {
            return Err(refused("Invalid retention intent"));
        }
        let mut names = BTreeSet::new();
        let mut allowed_dirs = BTreeSet::new();
        for f in &self.files {
            storage::relative_path(&f.path)?;
            let allowed = matches!(
                f.path.as_str(),
                OWNER | "journal.json" | "recovery-helper.exe" | "recovery-parent.json"
            ) || f
                .path
                .strip_prefix("files/")
                .is_some_and(|p| payload_path(p) || generated_path(p));
            if !allowed || !names.insert(f.path.to_ascii_lowercase()) {
                return Err(refused("Invalid retention file"));
            }
            validate_identity(&f.identity)?;
            ancestors(&f.path, &mut allowed_dirs);
        }
        if !names.contains(OWNER.to_ascii_lowercase().as_str()) || !names.contains("journal.json") {
            return Err(refused("Retention intent lacks ownership evidence"));
        }
        allowed_dirs.insert("files".into());
        let mut dirs = BTreeSet::new();
        for d in &self.directories {
            if !allowed_dirs.contains(&d.path) || !dirs.insert(d.path.clone()) {
                return Err(refused("Invalid retention directory"));
            }
        }
        if !dirs.contains("") {
            return Err(refused("Retention intent lacks root"));
        }
        for path in &self.authorized_deletions {
            if let Some(dir) = path.strip_prefix("directory:") {
                if !dirs.contains(dir) {
                    return Err(refused("Invalid directory deletion intent"));
                }
            } else if !self.files.iter().any(|f| f.path == *path) {
                return Err(refused("Invalid file deletion intent"));
            }
        }
        if !self.finalizing
            && ["journal.json", OWNER, "directory:"]
                .iter()
                .any(|p| self.authorized_deletions.contains(*p))
        {
            return Err(refused("Final metadata deletion lacks completion intent"));
        }
        if self
            .deleting
            .as_ref()
            .is_some_and(|p| !self.authorized_deletions.contains(p))
        {
            return Err(refused("Deletion lacks durable authorization"));
        }
        Ok(())
    }
    fn persist(&self, path: &Path) -> Result<()> {
        let mut bytes = serde_json::to_vec_pretty(self)?.len() as u64;
        for entry in fs::read_dir(
            path.parent()
                .ok_or_else(|| refused("Receipt has no parent"))?,
        )? {
            let entry = entry?;
            if entry.path() != path {
                storage::plain_path(&entry.path())?;
                bytes = bytes
                    .checked_add(entry.metadata()?.len())
                    .ok_or_else(|| refused("Receipt bytes overflow"))?;
            }
        }
        if bytes > MAX_RECEIPT_BYTES {
            return Err(refused("Bounded retention evidence budget full"));
        }
        storage::atomic_json(path, self)?;
        let reread: Intent = storage::read_json(path)?;
        if serde_json::to_vec(self)? != serde_json::to_vec(&reread)? {
            return Err(repair("Retention intent readback differs"));
        }
        Ok(())
    }
    fn check_file(&self, root: &Path, record: &RetiredFile) -> Result<()> {
        let path = storage::owned_join(root, &record.path)?;
        if file_identity_optional(&path)?.is_some()
            && (FileIdentity::read(&path)? != record.identity
                || !same_permissions(&checked_permissions(&path)?, &record.permissions))
        {
            return Err(refused("Retention file identity/permissions changed"));
        }
        Ok(())
    }
    fn check_directory(&self, root: &Path, record: &RetiredDirectory) -> Result<()> {
        let path = if record.path.is_empty() {
            root.to_path_buf()
        } else {
            storage::owned_join(root, &record.path)?
        };
        storage::plain_path(&path)?;
        if path.exists()
            && (!path.is_dir()
                || !same_permissions(&checked_permissions(&path)?, &record.permissions))
        {
            return Err(refused("Retention directory identity/permissions changed"));
        }
        Ok(())
    }
    fn apply(&mut self, state: &Path, intent_path: &Path) -> Result<()> {
        self.validate(&self.installation, &self.transaction)?;
        let root = state.join(&self.transaction);
        storage::plain_path(&root)?;
        if !root.exists() {
            if self.authorized_deletions.contains("directory:") {
                self.completed = true;
                self.deleting = None;
                self.persist(intent_path)?;
                return faultpoint();
            }
            return Err(refused("Snapshot vanished without root deletion intent"));
        }
        let (files, dirs) = paths(&root)?;
        let expected_files: BTreeSet<_> = self.files.iter().map(|f| f.path.clone()).collect();
        let expected_dirs: BTreeSet<_> = self.directories.iter().map(|d| d.path.clone()).collect();
        if !files.is_subset(&expected_files) || !dirs.is_subset(&expected_dirs) {
            return Err(refused("Unknown entry in retiring snapshot"));
        }
        if expected_files
            .difference(&files)
            .any(|p| !self.authorized_deletions.contains(p))
            || expected_dirs.difference(&dirs).any(|p| {
                !self
                    .authorized_deletions
                    .contains(&format!("directory:{p}"))
            })
        {
            return Err(refused(
                "Recovery entry vanished without exact deletion intent",
            ));
        }
        for f in &self.files {
            self.check_file(&root, f)?;
        }
        for d in &self.directories {
            self.check_directory(&root, d)?;
        }
        // Identity checks happen before the first mutation and again immediately
        // before each deletion. A persisted full manifest authorizes replay of
        // exact bytes resurrected by interrupted filesystem deletion.
        let records = self.files.clone();
        for f in records
            .iter()
            .filter(|f| !matches!(f.path.as_str(), OWNER | "journal.json"))
        {
            self.delete_file(intent_path, &root, f)?;
        }
        let mut directories = self.directories.clone();
        directories.sort_by_key(|d| std::cmp::Reverse(d.path.len()));
        for d in directories.iter().filter(|d| !d.path.is_empty()) {
            self.delete_directory(intent_path, &root, d)?;
        }
        self.finalizing = true;
        self.deleting = None;
        self.persist(intent_path)?;
        faultpoint()?;
        for name in ["journal.json", OWNER] {
            let f = records
                .iter()
                .find(|f| f.path == name)
                .ok_or_else(|| refused("Missing retention metadata"))?;
            self.delete_file(intent_path, &root, f)?;
        }
        let d = directories
            .iter()
            .find(|d| d.path.is_empty())
            .ok_or_else(|| refused("Missing retention root"))?;
        self.delete_directory(intent_path, &root, d)?;
        self.completed = true;
        self.deleting = None;
        self.persist(intent_path)?;
        faultpoint()
    }
    fn delete_file(&mut self, intent_path: &Path, root: &Path, record: &RetiredFile) -> Result<()> {
        let path = storage::owned_join(root, &record.path)?;
        if !path.exists() {
            return Ok(());
        }
        let _pins = platform::pin_directories(path.parent().unwrap())?;
        self.deleting = Some(record.path.clone());
        self.authorized_deletions.insert(record.path.clone());
        self.persist(intent_path)?;
        faultpoint()?;
        self.check_file(root, record)?;
        fs::remove_file(&path)?;
        storage::sync_directory(path.parent().unwrap())?;
        faultpoint()
    }
    fn delete_directory(
        &mut self,
        intent_path: &Path,
        root: &Path,
        record: &RetiredDirectory,
    ) -> Result<()> {
        let path = if record.path.is_empty() {
            root.to_path_buf()
        } else {
            storage::owned_join(root, &record.path)?
        };
        if !path.exists() {
            return Ok(());
        }
        let _pins = platform::pin_directories(path.parent().unwrap())?;
        self.deleting = Some(format!("directory:{}", record.path));
        self.authorized_deletions
            .insert(format!("directory:{}", record.path));
        self.persist(intent_path)?;
        faultpoint()?;
        self.check_directory(root, record)?;
        fs::remove_dir(&path)?; // Fails if any unknown/late entry exists.
        storage::sync_directory(path.parent().unwrap())?;
        faultpoint()
    }
}
fn retention_directory(state: &Path) -> Result<PathBuf> {
    let directory = state.join(DIRECTORY);
    storage::plain_path(&directory)?;
    if !directory.exists() {
        let _new_directory = platform::create_private_data_directory_new(&directory)?;
        storage::atomic_bytes(&directory.join(OWNER), PRODUCT_ID.as_bytes())?;
    }
    checked_permissions(&directory)?;
    if fs::read_to_string(directory.join(OWNER))? != PRODUCT_ID {
        return Err(refused("Retention receipt owner differs"));
    }
    Ok(directory)
}
fn ensure_current(state: &Path, current: &Journal) -> Result<()> {
    let Some(latest) = active(state, &current.installation)? else {
        return Err(refused("Retention active pointer vanished"));
    };
    if latest.journal.transaction != current.transaction
        || latest.journal.phase != Phase::Committed
        || serde_json::to_vec(&latest.journal.retention_link)?
            != serde_json::to_vec(&current.retention_link)?
    {
        return Err(refused("Retention active/predecessor changed"));
    }
    Ok(())
}
/// Caller owns maintenance.lock continuously; no active/current state can be
/// cooperatively changed until this pass returns. UI sessions remain shared.
pub(super) fn run(state: &Path, current: &Journal) -> Result<()> {
    if current.phase != Phase::Committed {
        return Ok(());
    }
    let Some(link) = &current.retention_link else {
        return Ok(());
    };
    if !name_valid(&current.transaction)
        || link
            .previous_terminal
            .as_ref()
            .is_some_and(|p| !name_valid(p) || p == &current.transaction)
    {
        return Err(refused("Invalid retention predecessor"));
    }
    storage::plain_path(state)?;
    checked_permissions(state)?;
    let _state_pins = platform::pin_directories(state)?;
    ensure_current(state, current)?;
    let keep =
        |name: &str| name == current.transaction || link.previous_terminal.as_deref() == Some(name);
    let mut candidates = BTreeSet::new();
    for (index, entry) in fs::read_dir(state)?.enumerate() {
        if index >= MAX_SETS {
            return Err(refused(
                "Too many updater-state entries for bounded retention",
            ));
        }
        let name = entry?
            .file_name()
            .into_string()
            .map_err(|_| refused("Invalid state entry"))?;
        if name_valid(&name) && !keep(&name) {
            candidates.insert(name);
        }
    }
    let receipts = retention_directory(state)?;
    let _receipt_pins = platform::pin_directories(&receipts)?;
    let mut receipt_count = 0usize;
    let mut receipt_bytes = 0u64;
    for (index, entry) in fs::read_dir(&receipts)?.enumerate() {
        if index >= MAX_SETS {
            return Err(refused("Too many retention receipts"));
        }
        let entry = entry?;
        storage::plain_path(&entry.path())?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| refused("Invalid receipt entry"))?;
        if name != OWNER {
            if !entry.metadata()?.is_file() || !name.strip_suffix(".json").is_some_and(name_valid) {
                return Err(refused("Unknown retention receipt entry"));
            }
            receipt_count += 1;
            receipt_bytes = receipt_bytes
                .checked_add(entry.metadata()?.len())
                .ok_or_else(|| refused("Receipt size overflow"))?;
        }
        if let Some(name) = name.strip_suffix(".json")
            && name_valid(name)
            && !keep(name)
        {
            candidates.insert(name.into());
        }
    }
    if receipt_count > MAX_RECEIPTS || receipt_bytes > MAX_RECEIPT_BYTES {
        return Err(refused("Retention receipts exceed bounded evidence budget"));
    }
    for name in candidates {
        ensure_current(state, current)?;
        let path = receipts.join(format!("{name}.json"));
        let mut intent = if path.exists() {
            checked_permissions(&path)?;
            let intent: Intent = storage::read_json(&path)?;
            if intent.installation != current.installation {
                continue;
            }
            intent.validate(&current.installation, &name)?;
            intent
        } else {
            let Some(intent) = capture(state, &current.installation, &name)? else {
                continue;
            };
            intent.validate(&current.installation, &name)?;
            let bytes = serde_json::to_vec_pretty(&intent)?.len() as u64;
            // Reserve room for exact per-deletion authorization and formatting.
            let reserved = bytes
                .checked_mul(2)
                .and_then(|n| n.checked_add(4096))
                .ok_or_else(|| refused("Receipt budget overflow"))?;
            if receipt_count >= MAX_RECEIPTS || receipt_bytes + reserved > MAX_RECEIPT_BYTES {
                return Err(refused(
                    "Retention receipt budget full; healthy version retained",
                ));
            }
            receipt_count += 1;
            receipt_bytes += reserved;
            intent.persist(&path)?;
            faultpoint()?;
            intent
        };
        intent.apply(state, &path)?;
    }
    Ok(())
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Warning {
    schema: u32,
    product_id: String,
    transaction: String,
    message: String,
}
pub(super) fn best_effort(state: &Path, current: &Journal) {
    if let Err(error) = run(state, current) {
        // Only this bounded private diagnostic may be replaced. Unknown files
        // at its name are preserved, and snapshots/receipts remain the evidence.
        let path = state.join("installer-retention-warning.json");
        let write = || -> Result<()> {
            storage::plain_path(&path)?;
            if path.exists() {
                checked_permissions(&path)?;
                let old: Warning = storage::read_json(&path)?;
                if old.schema != SCHEMA || old.product_id != PRODUCT_ID {
                    return Err(refused("Unowned retention warning"));
                }
            }
            let message: String =
                format!("Healthy installation retained. Old recovery cleanup deferred: {error}")
                    .chars()
                    .take(4096)
                    .collect();
            storage::atomic_json(
                &path,
                &Warning {
                    schema: SCHEMA,
                    product_id: PRODUCT_ID.into(),
                    transaction: current.transaction.clone(),
                    message,
                },
            )
        };
        let _ = write();
    }
}
