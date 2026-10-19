//! Persistent, product-scoped Windows integration recovery.
//!
//! No registry key, value name, or shortcut destination comes from a package or
//! remote manifest. The parent persists this snapshot before launching Setup and
//! again immediately after `seal_after`. Unsealed, unprovable changes fail closed.
#![cfg_attr(not(any(windows, test)), allow(dead_code))]
use super::{PRODUCT_ID, Result, UpdateError, platform, storage};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

const SCHEMA: u32 = 1;
const MAX_VALUE: usize = 65_536;
const MAX_SHORTCUT: usize = 131_072;
const MAX_TOTAL: usize = 524_288;
const ROOT: &str = r"Software\LightImageTool\DefaultApps";
const PROG: &str = r"Software\Classes\LightImageTool.Image";
const DISCOVERY: &str = r"Software\RegisteredApplications";
const EXTENSIONS: [&str; 5] = [".jpg", ".jpeg", ".png", ".gif", ".webp"];
const UNINSTALL_NAMES: [&str; 23] = [
    "DisplayName",
    "DisplayVersion",
    "InstallLocation",
    "DisplayIcon",
    "UninstallString",
    "QuietUninstallString",
    "Publisher",
    "Inno Setup: App Path",
    "Inno Setup: Icon Group",
    "Inno Setup: User",
    "Inno Setup: Selected Tasks",
    "Inno Setup: Deselected Tasks",
    "Inno Setup: Setup Version",
    "Inno Setup: Language",
    "Inno Setup: No Icons",
    "NoModify",
    "NoRepair",
    "EstimatedSize",
    "InstallDate",
    "MajorVersion",
    "MinorVersion",
    "VersionMajor",
    "VersionMinor",
];
fn security(message: impl Into<String>) -> UpdateError {
    UpdateError::Security(message.into())
}
fn repair(message: impl Into<String>) -> UpdateError {
    UpdateError::Repair(message.into())
}
fn uninstall_key() -> String {
    format!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{PRODUCT_ID}_is1")
}
fn menu_key(extension: &str) -> String {
    format!(r"Software\Classes\SystemFileAssociations\{extension}\shell\LightImageTool")
}

/// A missing value is `None`, distinct from REG_NONE with zero or more bytes.
/// No text decoding or type conversion occurs while capturing or restoring.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawValue {
    pub kind: u32,
    pub bytes: Vec<u8>,
}
impl RawValue {
    fn sz(value: &str) -> Self {
        Self {
            kind: 1,
            bytes: value
                .encode_utf16()
                .chain(Some(0))
                .flat_map(u16::to_le_bytes)
                .collect(),
        }
    }
    fn dword(value: u32) -> Self {
        Self {
            kind: 4,
            bytes: value.to_le_bytes().to_vec(),
        }
    }
    fn text(&self) -> Option<String> {
        if self.kind != 1 || self.bytes.len() < 2 || !self.bytes.len().is_multiple_of(2) {
            return None;
        }
        let units: Vec<u16> = self
            .bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        if units.last() != Some(&0) || units[..units.len() - 1].contains(&0) {
            return None;
        }
        String::from_utf16(&units[..units.len() - 1]).ok()
    }
    fn number(&self) -> Option<u32> {
        (self.kind == 4 && self.bytes.len() == 4)
            .then(|| u32::from_le_bytes(self.bytes[..4].try_into().unwrap()))
    }
    fn validate(&self) -> Result<()> {
        if self.bytes.len() > MAX_VALUE {
            return Err(security("Registry value exceeds recovery bound"));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyImage {
    sddl: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryRecord {
    key: String,
    name: String,
    before: Option<RawValue>,
    // `dynamic` is derived exclusively from a fixed local name allowlist.
    dynamic: bool,
    expected: Option<RawValue>,
    after: Option<RawValue>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyRecord {
    key: String,
    before: Option<KeyImage>,
    after: Option<KeyImage>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileImage {
    bytes: Vec<u8>,
    sha256: String,
    permissions: platform::FilePermissions,
}
impl FileImage {
    fn new(bytes: Vec<u8>, permissions: platform::FilePermissions) -> Self {
        let sha256 = format!("{:x}", Sha256::digest(&bytes));
        Self {
            bytes,
            sha256,
            permissions,
        }
    }
    fn validate(&self) -> Result<()> {
        if self.bytes.is_empty()
            || self.bytes.len() > MAX_SHORTCUT
            || self.sha256 != format!("{:x}", Sha256::digest(&self.bytes))
        {
            return Err(security("Shortcut preimage is incomplete or corrupt"));
        }
        validate_permissions(&self.permissions)
    }
}
fn validate_permissions(p: &platform::FilePermissions) -> Result<()> {
    if p.windows_sddl
        .as_ref()
        .is_some_and(|s| s.is_empty() || s.len() > MAX_VALUE || s.contains('\0'))
    {
        return Err(security("Invalid saved file security descriptor"));
    }
    #[cfg(windows)]
    if p.windows_sddl.is_none() || p.unix_mode.is_some() {
        return Err(security("Missing Windows file security descriptor"));
    }
    Ok(())
}
fn permissions_equal(a: &platform::FilePermissions, b: &platform::FilePermissions) -> bool {
    a.readonly == b.readonly && a.unix_mode == b.unix_mode && a.windows_sddl == b.windows_sddl
}
fn files_equal(a: &Option<FileImage>, b: &Option<FileImage>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            a.bytes == b.bytes
                && a.sha256 == b.sha256
                && permissions_equal(&a.permissions, &b.permissions)
        }
        _ => false,
    }
}
fn directory_equal(
    a: &Option<platform::FilePermissions>,
    b: &Option<platform::FilePermissions>,
) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => permissions_equal(a, b),
        _ => false,
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ShortcutRecord {
    legacy: bool,
    before: Option<FileImage>,
    after: Option<FileImage>,
    directory_before: Option<platform::FilePermissions>,
    directory_after: Option<platform::FilePermissions>,
    other_entries: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    schema: u32,
    product_id: String,
    installation: PathBuf,
    programs: PathBuf,
    fixture: bool,
    installer_user: String,
    explorer: bool,
    version: String,
    sealed: bool,
    write_intent: bool,
    registry: Vec<RegistryRecord>,
    keys: Vec<KeyRecord>,
    shortcuts: Vec<ShortcutRecord>,
}

/// All mutation logic uses this backend, including Windows. Tests implement it
/// solely in memory: no test calls the native registry or writes user shortcuts.
trait Backend {
    fn programs(&self) -> &Path;
    fn user_name(&self) -> Result<String>;
    fn value(&mut self, key: &str, name: &str) -> Result<Option<RawValue>>;
    fn key(&mut self, key: &str) -> Result<Option<KeyImage>>;
    fn contents(&mut self, key: &str) -> Result<(Vec<String>, Vec<String>)>;
    fn create_key(&mut self, key: &str) -> Result<()>;
    fn set_value(&mut self, key: &str, name: &str, value: &Option<RawValue>) -> Result<()>;
    fn set_security(&mut self, key: &str, image: &KeyImage) -> Result<()>;
    fn delete_empty_key(&mut self, key: &str) -> Result<()>;
    fn shortcut(&mut self, legacy: bool) -> Result<Option<FileImage>>;
    fn shortcut_owned(&mut self, legacy: bool, installation: &Path) -> Result<bool>;
    fn directory_other_entries(&mut self, legacy: bool) -> Result<Vec<String>>;
    fn shortcut_directory(&mut self, legacy: bool) -> Result<Option<platform::FilePermissions>>;
    fn put_shortcut(&mut self, legacy: bool, image: &Option<FileImage>) -> Result<()>;
    fn restore_directory(
        &mut self,
        legacy: bool,
        image: &Option<platform::FilePermissions>,
    ) -> Result<()>;
}
fn fixed_keys() -> Vec<String> {
    let mut result: BTreeSet<String> = [
        uninstall_key(),
        ROOT.into(),
        format!(r"{ROOT}\Capabilities"),
        format!(r"{ROOT}\Capabilities\FileAssociations"),
        PROG.into(),
        format!(r"{PROG}\DefaultIcon"),
        format!(r"{PROG}\shell"),
        format!(r"{PROG}\shell\open"),
        format!(r"{PROG}\shell\open\command"),
    ]
    .into_iter()
    .collect();
    for ext in EXTENSIONS {
        let k = menu_key(ext);
        result.insert(format!(r"{k}\command"));
        result.insert(k);
    }
    result.into_iter().collect()
}
fn string_path(path: &Path) -> Result<String> {
    let s = path
        .to_str()
        .ok_or_else(|| security("Windows path is not valid Unicode"))?;
    if s.len() > 4096 || s.contains(['\0', '"', '\r', '\n']) {
        return Err(security("Unsafe installation path"));
    }
    Ok(s.to_owned())
}
type ExpectedValues = BTreeMap<(String, String), (bool, Option<RawValue>)>;
fn expected_values(installation: &Path, explorer: bool, version: &str) -> Result<ExpectedValues> {
    let exe = string_path(&installation.join("gloamere-image.exe"))?;
    let icon = string_path(&installation.join("assets").join("gloamere-image.ico"))?;
    let app = string_path(installation)?;
    let mut map = BTreeMap::new();
    let mut put = |key: &str, name: &str, text: Option<&str>| {
        map.insert(
            (key.to_owned(), name.to_owned()),
            (false, text.map(RawValue::sz)),
        );
    };
    for key in [ROOT, PROG] {
        put(key, "OwnerId", Some(PRODUCT_ID));
        put(key, "OwnerPath", Some(&exe));
    }
    let capabilities = format!(r"{ROOT}\Capabilities");
    put(&capabilities, "ApplicationName", Some("gloamere-image"));
    put(
        &capabilities,
        "ApplicationDescription",
        Some("Local JPEG, PNG, GIF and WebP viewer with static-copy editing."),
    );
    put(
        &capabilities,
        "ApplicationIcon",
        Some(&format!("\"{icon}\",0")),
    );
    for ext in EXTENSIONS {
        put(
            &format!(r"{capabilities}\FileAssociations"),
            ext,
            Some("LightImageTool.Image"),
        );
        let k = menu_key(ext);
        for (name, text) in [
            ("OwnerId", PRODUCT_ID.to_owned()),
            ("OwnerPath", exe.clone()),
            ("MUIVerb", "Open with gloamere-image".into()),
            ("Icon", format!("\"{icon}\",0")),
            ("MultiSelectModel", "Single".into()),
        ] {
            put(&k, name, explorer.then_some(text.as_str()));
        }
        put(
            &format!(r"{k}\command"),
            "",
            explorer.then_some(format!("\"{exe}\" --open \"%1\"").as_str()),
        );
    }
    put(
        PROG,
        "",
        Some("gloamere-image JPEG / PNG / GIF / WebP image"),
    );
    put(
        &format!(r"{PROG}\DefaultIcon"),
        "",
        Some(&format!("\"{icon}\",0")),
    );
    put(
        &format!(r"{PROG}\shell\open\command"),
        "",
        Some(&format!("\"{exe}\" --open \"%1\"")),
    );
    put(DISCOVERY, "gloamere-image", Some(&capabilities));
    put(DISCOVERY, "Light Image Tool", None);
    let uninstall = uninstall_key();
    for name in UNINSTALL_NAMES {
        map.insert((uninstall.clone(), name.into()), (true, None));
    }
    let mut put = |name: &str, value: Option<RawValue>| {
        map.insert((uninstall.clone(), name.into()), (false, value));
    };
    for (name, text) in [
        (
            "DisplayName",
            format!("gloamere-image {version} (unsigned test)"),
        ),
        ("DisplayVersion", version.into()),
        ("Publisher", "gloamere-image contributors".into()),
        (
            "InstallLocation",
            format!("{}\\", app.trim_end_matches('\\')),
        ),
        ("Inno Setup: App Path", app),
        ("DisplayIcon", icon),
        ("Inno Setup: Icon Group", "gloamere-image".into()),
        (
            "Inno Setup: Selected Tasks",
            if explorer { "explorer" } else { "" }.into(),
        ),
        (
            "Inno Setup: Deselected Tasks",
            if explorer { "" } else { "explorer" }.into(),
        ),
    ] {
        put(name, Some(RawValue::sz(&text)));
    }
    for name in ["NoModify", "NoRepair"] {
        put(name, Some(RawValue::dword(1)));
    }
    // The supported install does not offer a No Icons option.
    put("Inno Setup: No Icons", None);
    let mut parts = version.split('.');
    for (names, value) in [
        (["MajorVersion", "VersionMajor"], parts.next()),
        (["MinorVersion", "VersionMinor"], parts.next()),
    ] {
        let number = value
            .and_then(|v| v.parse::<u32>().ok())
            .ok_or_else(|| security("Invalid installer version"))?;
        for name in names {
            put(name, Some(RawValue::dword(number)));
        }
    }
    Ok(map)
}
impl Snapshot {
    pub fn capture(
        installation: &Path,
        directory: &Path,
        explorer: bool,
        version: &str,
    ) -> Result<Self> {
        validate_paths(installation, directory)?;
        #[cfg(all(windows, not(test)))]
        {
            Self::capture_with(&mut native::Native::new()?, installation, explorer, version)
        }
        #[cfg(any(not(windows), test))]
        {
            let snapshot = Self {
                schema: SCHEMA,
                product_id: PRODUCT_ID.into(),
                installation: installation.to_owned(),
                programs: directory.join("fixture-programs"),
                fixture: true,
                installer_user: "portable-fixture".into(),
                explorer,
                version: version.into(),
                sealed: false,
                write_intent: false,
                registry: Vec::new(),
                keys: Vec::new(),
                shortcuts: Vec::new(),
            };
            snapshot.validate_structure(installation)?;
            Ok(snapshot)
        }
    }
    pub fn validate(&self, installation: &Path, directory: &Path) -> Result<()> {
        validate_paths(installation, directory)?;
        self.validate_structure(installation)?;
        #[cfg(all(windows, not(test)))]
        if native::programs_directory()? != self.programs
            || native::current_user_name()? != self.installer_user
        {
            return Err(security(
                "Start Menu known folder changed; preserve recovery evidence",
            ));
        }
        Ok(())
    }
    /// The caller must persist the containing journal and hold all cooperative
    /// installer/session locks before any Setup mutation. This authorizes only
    /// restoring already-owned preimages after a torn write, never new objects.
    pub fn mark_write_intent(&mut self) -> Result<()> {
        self.validate_structure(&self.installation)?;
        if self.sealed {
            return Err(security("Cannot begin a sealed integration snapshot"));
        }
        self.write_intent = true;
        Ok(())
    }
    /// Native migration replaces the legacy in-memory Inno deletion. The parent
    /// calls this only after its Installing journal (including write_intent) is
    /// durable, while holding the installation/session locks.
    pub fn remove_legacy_shortcut(&self, installation: &Path, directory: &Path) -> Result<()> {
        self.validate(installation, directory)?;
        if !self.write_intent || self.sealed {
            return Err(security(
                "Legacy migration requires unsealed durable write intent",
            ));
        }
        #[cfg(all(windows, not(test)))]
        {
            self.remove_legacy_with(&mut native::Native::new()?)
        }
        #[cfg(any(not(windows), test))]
        {
            if !self.fixture {
                return Err(UpdateError::Deferred(
                    "Native legacy shortcut migration requires Windows".into(),
                ));
            }
            Ok(())
        }
    }
    pub fn seal_after(&mut self, installation: &Path, directory: &Path) -> Result<()> {
        self.validate(installation, directory)?;
        #[cfg(all(windows, not(test)))]
        {
            self.seal_with(&mut native::Native::new()?)
        }
        #[cfg(any(not(windows), test))]
        {
            if !self.fixture || !self.write_intent {
                return Err(UpdateError::Deferred(
                    "Only an explicitly begun portable fixture can be sealed here".into(),
                ));
            }
            self.sealed = true;
            Ok(())
        }
    }
    /// Read-only global preflight for the parent transaction. Call this before
    /// restoring any payload file; `restore` repeats it before integration writes.
    pub fn preflight_restore(&self, installation: &Path, directory: &Path) -> Result<()> {
        self.validate(installation, directory)?;
        #[cfg(all(windows, not(test)))]
        {
            self.check_live(&mut native::Native::new()?)
        }
        #[cfg(any(not(windows), test))]
        {
            if !self.fixture {
                return Err(UpdateError::Deferred(
                    "Native integration preflight requires Windows".into(),
                ));
            }
            Ok(())
        }
    }
    pub fn restore(&self, installation: &Path, directory: &Path) -> Result<()> {
        self.validate(installation, directory)?;
        #[cfg(all(windows, not(test)))]
        {
            self.restore_with(&mut native::Native::new()?)
        }
        #[cfg(any(not(windows), test))]
        {
            if !self.fixture {
                return Err(UpdateError::Deferred(
                    "Native integration restore requires Windows".into(),
                ));
            }
            Ok(())
        }
    }
    fn remove_legacy_with(&self, backend: &mut impl Backend) -> Result<()> {
        self.validate_structure(&self.installation)?;
        if !self.write_intent || self.sealed || backend.programs() != self.programs {
            return Err(security(
                "Legacy migration lacks its original durable write intent/location",
            ));
        }
        let record = self
            .shortcuts
            .iter()
            .find(|s| s.legacy)
            .ok_or_else(|| security("Missing legacy shortcut preimage"))?;
        if backend.directory_other_entries(true)? != record.other_entries {
            return Err(repair("Legacy shortcut siblings changed before migration"));
        }
        let current = backend.shortcut(true)?;
        let directory = backend.shortcut_directory(true)?;
        if current.is_some() {
            if !files_equal(&current, &record.before)
                || !directory_equal(&directory, &record.directory_before)
                || !backend.shortcut_owned(true, &self.installation)?
            {
                return Err(repair(
                    "Legacy shortcut changed or was retargeted; migration refused",
                ));
            }
            // Recheck after COM target inspection and immediately before removal.
            if !files_equal(&backend.shortcut(true)?, &record.before) {
                return Err(repair("Legacy shortcut changed during target inspection"));
            }
            backend.put_shortcut(true, &None)?;
        } else if !(directory_equal(&directory, &record.directory_before)
            || (record.before.is_some() && directory.is_none() && record.other_entries.is_empty()))
        {
            return Err(repair("Legacy shortcut directory changed before migration"));
        }
        // A group we merely found empty, without ever owning its link, is not ours
        // to remove. Preserve groups containing unrelated user files as well.
        if record.before.is_some() && record.other_entries.is_empty() {
            backend.restore_directory(true, &None)?;
        }
        if backend.shortcut(true)?.is_some()
            || backend.directory_other_entries(true)? != record.other_entries
        {
            return Err(repair("Legacy shortcut migration readback differs"));
        }
        let expected_directory = if record.before.is_some() && record.other_entries.is_empty() {
            None
        } else {
            record.directory_before.clone()
        };
        if !directory_equal(&backend.shortcut_directory(true)?, &expected_directory) {
            return Err(repair("Legacy shortcut directory readback differs"));
        }
        Ok(())
    }
    fn capture_with(
        backend: &mut impl Backend,
        installation: &Path,
        explorer: bool,
        version: &str,
    ) -> Result<Self> {
        let expected = expected_values(installation, explorer, version)?;
        let mut registry = Vec::new();
        for ((key, name), (dynamic, expected)) in expected {
            let before = backend.value(&key, &name)?;
            registry.push(RegistryRecord {
                key,
                name,
                before,
                dynamic,
                expected,
                after: None,
            });
        }
        let mut keys = Vec::new();
        for key in fixed_keys() {
            keys.push(KeyRecord {
                before: backend.key(&key)?,
                key,
                after: None,
            });
        }
        let mut shortcuts = Vec::new();
        for legacy in [false, true] {
            let before = backend.shortcut(legacy)?;
            if before.is_some() && !backend.shortcut_owned(legacy, installation)? {
                return Err(security(
                    "Foreign product-named shortcut; no installation changes allowed",
                ));
            }
            shortcuts.push(ShortcutRecord {
                legacy,
                before,
                after: None,
                directory_before: backend.shortcut_directory(legacy)?,
                directory_after: None,
                other_entries: backend.directory_other_entries(legacy)?,
            });
        }
        let snapshot = Self {
            schema: SCHEMA,
            product_id: PRODUCT_ID.into(),
            installation: installation.to_owned(),
            programs: backend.programs().to_owned(),
            fixture: false,
            installer_user: backend.user_name()?,
            explorer,
            version: version.into(),
            sealed: false,
            write_intent: false,
            registry,
            keys,
            shortcuts,
        };
        snapshot.validate_structure(installation)?;
        snapshot.verify_contents(backend)?;
        snapshot.verify_before(backend)?;
        Ok(snapshot)
    }
    fn validate_structure(&self, installation: &Path) -> Result<()> {
        if self.schema != SCHEMA
            || self.product_id != PRODUCT_ID
            || self.installation != installation
            || !self.programs.is_absolute()
            || self.installer_user.is_empty()
            || self.installer_user.len() > 1024
            || self.installer_user.chars().any(char::is_control)
            || self.version.len() > 64
            || self.version.split('.').count() != 3
            || !self
                .version
                .split('.')
                .all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(security("Integration snapshot binding or schema mismatch"));
        }
        storage::plain_path(&self.programs)?;
        if self.fixture {
            #[cfg(all(windows, not(test)))]
            {
                return Err(security(
                    "Portable fixture cannot authorize Windows integration recovery",
                ));
            }
            #[cfg(any(not(windows), test))]
            {
                if !self.registry.is_empty() || !self.keys.is_empty() || !self.shortcuts.is_empty()
                {
                    return Err(security(
                        "Portable fixture contains native integration state",
                    ));
                }
                return Ok(());
            }
        }
        let expected = expected_values(installation, self.explorer, &self.version)?;
        if self.registry.len() != expected.len()
            || self.keys.len() != fixed_keys().len()
            || self.shortcuts.len() != 2
        {
            return Err(security("Integration snapshot is incomplete"));
        }
        let mut seen = BTreeSet::new();
        let mut total = 0usize;
        for value in &self.registry {
            if !seen.insert((&value.key, &value.name))
                || expected.get(&(value.key.clone(), value.name.clone()))
                    != Some(&(value.dynamic, value.expected.clone()))
            {
                return Err(security(
                    "Unexpected, duplicated, or changed registry recovery target",
                ));
            }
            for raw in [&value.before, &value.expected, &value.after]
                .into_iter()
                .flatten()
            {
                raw.validate()?;
                total = total
                    .checked_add(raw.bytes.len())
                    .ok_or_else(|| security("Snapshot size overflow"))?;
            }
            if !self.sealed && value.after.is_some() {
                return Err(security(
                    "Unsealed snapshot contains adopted registry values",
                ));
            }
            if self.sealed && !value.dynamic && value.after != value.expected {
                return Err(security(
                    "Sealed registry value differs from predeclared installer value",
                ));
            }
            if self.sealed && value.dynamic && !self.dynamic_value_allowed(value, &value.after) {
                return Err(security(
                    "Sealed uninstall value is not an owned installer postimage",
                ));
            }
        }
        let expected_keys: BTreeSet<_> = fixed_keys().into_iter().collect();
        let mut seen = BTreeSet::new();
        for record in &self.keys {
            if !expected_keys.contains(&record.key) || !seen.insert(&record.key) {
                return Err(security(
                    "Unexpected or duplicate registry key recovery target",
                ));
            }
            for image in [&record.before, &record.after].into_iter().flatten() {
                if image.sddl.is_empty()
                    || image.sddl.len() > MAX_VALUE
                    || image.sddl.contains('\0')
                {
                    return Err(security("Invalid registry security preimage"));
                }
                total += image.sddl.len();
            }
            if !self.sealed && record.after.is_some() {
                return Err(security("Unsealed registry security postimage"));
            }
        }
        let mut seen = BTreeSet::new();
        for record in &self.shortcuts {
            if !seen.insert(record.legacy) {
                return Err(security("Duplicate shortcut recovery target"));
            }
            if record.other_entries.len() > 64
                || record.other_entries.windows(2).any(|w| w[0] >= w[1])
                || record
                    .other_entries
                    .iter()
                    .any(|s| s.len() > 1024 || s.contains(['\\', '/', '\0']))
            {
                return Err(security("Invalid shortcut directory sibling snapshot"));
            }
            for image in [&record.before, &record.after].into_iter().flatten() {
                image.validate()?;
                total += image.bytes.len();
            }
            for image in [&record.directory_before, &record.directory_after]
                .into_iter()
                .flatten()
            {
                validate_permissions(image)?;
            }
            if !self.sealed && (record.after.is_some() || record.directory_after.is_some()) {
                return Err(security("Unsealed shortcut postimage"));
            }
            if self.sealed
                && ((!record.legacy && record.after.is_none())
                    || (record.legacy && record.after.is_some()))
            {
                return Err(security("Invalid installed shortcut state"));
            }
        }
        if total > MAX_TOTAL {
            return Err(security("Integration snapshot exceeds aggregate bound"));
        }
        for record in &self.registry {
            if record.key != DISCOVERY
                && record.before.is_some()
                && !self
                    .keys
                    .iter()
                    .any(|k| k.key == record.key && k.before.is_some())
            {
                return Err(security("Saved registry value has no original key"));
            }
        }
        for record in &self.keys {
            if record.before.is_some()
                && let Some((parent, _)) = record.key.rsplit_once('\\')
                && self
                    .keys
                    .iter()
                    .any(|k| k.key == parent && k.before.is_none())
            {
                return Err(security(
                    "Saved registry child has no original product parent",
                ));
            }
            if self.sealed {
                let required = self.registry.iter().any(|v| {
                    v.after.is_some()
                        && (v.key == record.key || v.key.starts_with(&format!(r"{}\", record.key)))
                });
                if required != record.after.is_some() {
                    return Err(security(
                        "Sealed registry key existence differs from product postimage",
                    ));
                }
            }
        }
        for record in &self.shortcuts {
            if record.before.is_some() && record.directory_before.is_none() {
                return Err(security("Shortcut preimage has no original directory"));
            }
        }
        self.verify_initial_ownership()?;
        Ok(())
    }
    fn before(&self, key: &str, name: &str) -> Option<&RawValue> {
        self.registry
            .iter()
            .find(|r| r.key == key && r.name == name)
            .and_then(|r| r.before.as_ref())
    }
    fn verify_initial_ownership(&self) -> Result<()> {
        let exe = string_path(&self.installation.join("gloamere-image.exe"))?;
        let legacy = string_path(&self.installation.join("light-image-tool.exe"))?;
        let owner = |key: &str| {
            self.before(key, "OwnerId") == Some(&RawValue::sz(PRODUCT_ID))
                && self
                    .before(key, "OwnerPath")
                    .and_then(RawValue::text)
                    .is_some_and(|p| {
                        p.eq_ignore_ascii_case(&exe) || p.eq_ignore_ascii_case(&legacy)
                    })
        };
        for key in [ROOT.to_owned(), PROG.to_owned()]
            .into_iter()
            .chain(EXTENSIONS.map(menu_key))
        {
            if self.keys.iter().any(|r| r.key == key && r.before.is_some()) && !owner(&key) {
                return Err(security("Product registration has foreign ownership"));
            }
        }
        for name in ["gloamere-image", "Light Image Tool"] {
            if let Some(raw) = self.before(DISCOVERY, name)
                && (raw != &RawValue::sz(&format!(r"{ROOT}\Capabilities")) || !owner(ROOT))
            {
                return Err(security("Foreign discovery value occupies product name"));
            }
        }
        if self
            .keys
            .iter()
            .any(|r| r.key == uninstall_key() && r.before.is_some())
        {
            let app = string_path(&self.installation)?;
            if !self
                .before(&uninstall_key(), "Inno Setup: App Path")
                .and_then(RawValue::text)
                .is_some_and(|p| p.eq_ignore_ascii_case(&app))
                || !self
                    .before(&uninstall_key(), "UninstallString")
                    .and_then(RawValue::text)
                    .is_some_and(|s| valid_uninstall_command(&self.installation, &s, false))
            {
                return Err(security("Uninstall key belongs to another installation"));
            }
        }
        Ok(())
    }
    fn verify_contents(&self, backend: &mut impl Backend) -> Result<()> {
        for record in &self.keys {
            let (values, children) = backend.contents(&record.key)?;
            if values.len() > UNINSTALL_NAMES.len() + 8 || children.len() > 8 {
                return Err(repair("Product key has unexpected contents; retained"));
            }
            let mut seen = BTreeSet::new();
            for name in values {
                if !seen.insert(name.to_lowercase())
                    || !self
                        .registry
                        .iter()
                        .any(|v| v.key == record.key && v.name.eq_ignore_ascii_case(&name))
                {
                    return Err(repair(format!(
                        "Foreign registry value retained in {}",
                        record.key
                    )));
                }
            }
            let mut seen = BTreeSet::new();
            for name in children {
                if !seen.insert(name.to_lowercase())
                    || !self.keys.iter().any(|v| {
                        v.key
                            .eq_ignore_ascii_case(&format!(r"{}\{name}", record.key))
                    })
                {
                    return Err(repair(format!(
                        "Foreign registry child retained in {}",
                        record.key
                    )));
                }
            }
        }
        Ok(())
    }
    fn dynamic_value_allowed(&self, record: &RegistryRecord, value: &Option<RawValue>) -> bool {
        if !record.dynamic || record.key != uninstall_key() {
            return false;
        }
        let Some(value) = value else {
            return false;
        };
        match record.name.as_str() {
            "UninstallString" | "QuietUninstallString" => value.text().is_some_and(|s| {
                valid_uninstall_command(
                    &self.installation,
                    &s,
                    record.name == "QuietUninstallString",
                )
            }),
            "Inno Setup: Setup Version" => value.text().is_some_and(|s| {
                s.len() <= 32
                    && s.split('.').count() >= 2
                    && s.bytes().all(|b| b.is_ascii_digit() || b == b'.')
            }),
            "Inno Setup: Language" => value
                .text()
                .is_some_and(|s| matches!(s.as_str(), "english" | "chinesesimp")),
            // Derived from the same Win32 username API as Setup before writes;
            // this also works when a clean install has no uninstall preimage.
            "Inno Setup: User" => value == &RawValue::sz(&self.installer_user),
            "InstallDate" => value.text().is_some_and(|s| {
                s.len() == 8
                    && s.bytes().all(|b| b.is_ascii_digit())
                    && s[0..4]
                        .parse::<u32>()
                        .is_ok_and(|n| (2000..=9999).contains(&n))
                    && s[4..6].parse::<u32>().is_ok_and(|n| (1..=12).contains(&n))
                    && s[6..8].parse::<u32>().is_ok_and(|n| (1..=31).contains(&n))
            }),
            "EstimatedSize" => value
                .number()
                .is_some_and(|n| n > 0 && n <= 16 * 1024 * 1024),
            _ => false,
        }
    }
    fn seal_with(&mut self, backend: &mut impl Backend) -> Result<()> {
        self.validate_structure(&self.installation)?;
        if backend.programs() != self.programs {
            return Err(security("Known folder no longer matches snapshot"));
        }
        self.verify_contents(backend)?;
        if !self.write_intent {
            return Err(security("Missing persisted installer write intent"));
        }
        if self.sealed {
            return self.verify_sealed(backend);
        }
        let mut sealed = self.clone();
        for record in &mut sealed.registry {
            let current = backend.value(&record.key, &record.name)?;
            if (record.dynamic && !self.dynamic_value_allowed(record, &current))
                || (!record.dynamic && current != record.expected)
            {
                return Err(repair(format!(
                    "Setup produced unexpected registry value {}\\{}",
                    record.key, record.name
                )));
            }
            record.after = current;
        }
        for record in &mut sealed.keys {
            record.after = backend.key(&record.key)?;
            let required = sealed.registry.iter().any(|v| {
                v.after.is_some()
                    && (v.key == record.key || v.key.starts_with(&format!(r"{}\", record.key)))
            });
            if required != record.after.is_some() {
                return Err(repair("Unexpected registry key presence after Setup"));
            }
            // Ordinary profile writes cannot change ACLs. Inno recreates only its
            // own uninstall key; its postimage is sealed after ownership checks.
            if record.key != uninstall_key()
                && record.before.is_some()
                && record.after.is_some()
                && record.before != record.after
            {
                return Err(repair("Registry ACL changed concurrently; retain evidence"));
            }
        }
        for record in &mut sealed.shortcuts {
            if backend.directory_other_entries(record.legacy)? != record.other_entries {
                return Err(repair("Start Menu siblings changed while Setup ran"));
            }
            record.after = backend.shortcut(record.legacy)?;
            record.directory_after = backend.shortcut_directory(record.legacy)?;
            if record.legacy {
                if record.after.is_some() {
                    return Err(repair("Legacy shortcut was not removed by Setup"));
                }
            } else if record.after.is_none()
                || !backend.shortcut_owned(false, &self.installation)?
            {
                return Err(repair(
                    "Installed shortcut does not target the stable product launcher",
                ));
            }
        }
        sealed.sealed = true;
        sealed.validate_structure(&sealed.installation)?;
        sealed.verify_sealed(backend)?;
        *self = sealed;
        Ok(())
    }
    fn verify_sealed(&self, backend: &mut impl Backend) -> Result<()> {
        for record in &self.registry {
            if backend.value(&record.key, &record.name)? != record.after {
                return Err(repair("Registry changed during postimage sealing"));
            }
        }
        for record in &self.keys {
            if backend.key(&record.key)? != record.after {
                return Err(repair("Registry security changed during postimage sealing"));
            }
        }
        for record in &self.shortcuts {
            if !files_equal(&backend.shortcut(record.legacy)?, &record.after)
                || !directory_equal(
                    &backend.shortcut_directory(record.legacy)?,
                    &record.directory_after,
                )
            {
                return Err(repair("Shortcut changed during postimage sealing"));
            }
        }
        Ok(())
    }
    fn value_admissible(&self, record: &RegistryRecord, current: &Option<RawValue>) -> bool {
        current == &record.before
            || (self.sealed && current == &record.after)
            || (!self.sealed && !record.dynamic && current == &record.expected)
            || (self.write_intent
                && !self.sealed
                && record.before.is_some()
                && record.key != DISCOVERY
                && self
                    .keys
                    .iter()
                    .any(|k| k.key == record.key && k.before.is_some())
                && (current.is_none()
                    || !matches!(
                        record.name.as_str(),
                        "OwnerId"
                            | "OwnerPath"
                            | "UninstallString"
                            | "QuietUninstallString"
                            | "Inno Setup: App Path"
                    )))
    }
    fn key_admissible(&self, record: &KeyRecord, current: &Option<KeyImage>) -> bool {
        if current == &record.before || (self.sealed && current == &record.after) {
            return true;
        }
        // Inno deletes/recreates this exclusive key as part of its documented
        // write sequence. Only the persisted begin intent authorizes recovering
        // its already-owned preimage while the key is missing.
        if self.write_intent
            && !self.sealed
            && record.before.is_some()
            && record.key == uninstall_key()
        {
            return true;
        }
        // Predeclared Explorer removal includes the whole exclusive key.
        !self.sealed
            && current.is_none()
            && !self.explorer
            && EXTENSIONS.iter().any(|e| {
                record.key == menu_key(e) || record.key == format!(r"{}\command", menu_key(e))
            })
    }
    fn shortcut_admissible(&self, record: &ShortcutRecord, current: &Option<FileImage>) -> bool {
        files_equal(current, &record.before)
            || (self.sealed && files_equal(current, &record.after))
            || (!self.sealed && record.legacy && current.is_none())
            || (self.write_intent && !self.sealed && record.before.is_some())
    }
    fn check_live(&self, backend: &mut impl Backend) -> Result<()> {
        if backend.programs() != self.programs {
            return Err(repair("Start Menu location changed; retain recovery data"));
        }
        self.verify_contents(backend)?;
        for record in &self.registry {
            if !self.value_admissible(record, &backend.value(&record.key, &record.name)?) {
                return Err(repair(format!(
                    "Foreign or unsealed registry change retained: {}\\{}",
                    record.key, record.name
                )));
            }
        }
        for record in &self.keys {
            if !self.key_admissible(record, &backend.key(&record.key)?) {
                return Err(repair(format!(
                    "Foreign or unsealed registry key/security retained: {}",
                    record.key
                )));
            }
        }
        for record in &self.shortcuts {
            if backend.directory_other_entries(record.legacy)? != record.other_entries {
                return Err(repair(
                    "Foreign Start Menu group contents changed; retained",
                ));
            }
            let current_file = backend.shortcut(record.legacy)?;
            if !self.shortcut_admissible(record, &current_file) {
                return Err(repair("Foreign or unsealed shortcut retained"));
            }
            self.verify_unsealed_shortcut(backend, record, &current_file)?;
            let current = backend.shortcut_directory(record.legacy)?;
            let admissible = directory_equal(&current, &record.directory_before)
                || (self.sealed && directory_equal(&current, &record.directory_after))
                || (!self.sealed
                    && current.is_none()
                    && (record.legacy || (self.write_intent && record.directory_before.is_some())));
            if !admissible {
                return Err(repair(
                    "Foreign or unsealed shortcut directory ACL retained",
                ));
            }
        }
        Ok(())
    }
    fn verify_unsealed_shortcut(
        &self,
        backend: &mut impl Backend,
        record: &ShortcutRecord,
        current: &Option<FileImage>,
    ) -> Result<()> {
        if self.write_intent
            && !self.sealed
            && record.before.is_some()
            && !files_equal(current, &record.before)
            && let Some(image) = current
        {
            // A complete Shell Link header warrants target inspection. A
            // torn/empty old-owned file is covered by durable write intent;
            // a valid link retargeted to somebody else's program is not.
            const LINK_CLSID: [u8; 16] = [1, 20, 2, 0, 0, 0, 0, 0, 192, 0, 0, 0, 0, 0, 0, 70];
            let complete = image.bytes.len() >= 76
                && image.bytes[..4] == [76, 0, 0, 0]
                && image.bytes[4..20] == LINK_CLSID;
            if complete && !backend.shortcut_owned(record.legacy, &self.installation)? {
                return Err(repair(
                    "Old shortcut was retargeted outside the installation; retained",
                ));
            }
        }
        Ok(())
    }
    fn restore_with(&self, backend: &mut impl Backend) -> Result<()> {
        self.validate_structure(&self.installation)?;
        // Crucially, nothing is written if any unrelated change already exists.
        self.check_live(backend)?;
        for record in &self.keys {
            if let Some(before) = &record.before
                && backend.key(&record.key)?.is_none()
            {
                backend.create_key(&record.key)?;
                backend.set_security(&record.key, before)?;
                if backend.key(&record.key)? != record.before {
                    return Err(repair("Recreated key security readback differs"));
                }
            }
        }
        for record in &self.registry {
            let current = backend.value(&record.key, &record.name)?;
            if !self.value_admissible(record, &current) {
                return Err(repair("Registry changed immediately before restore"));
            }
            if current != record.before {
                backend.set_value(&record.key, &record.name, &record.before)?;
            }
            if backend.value(&record.key, &record.name)? != record.before {
                return Err(repair("Restored registry raw type/bytes differ"));
            }
        }
        for record in &self.keys {
            let current = backend.key(&record.key)?;
            if !self.key_admissible(record, &current) {
                return Err(repair(
                    "Registry security changed immediately before restore",
                ));
            }
            if let Some(before) = &record.before
                && current.as_ref() != Some(before)
            {
                backend.set_security(&record.key, before)?;
            }
        }
        for record in self.keys.iter().rev() {
            if record.before.is_none() && backend.key(&record.key)?.is_some() {
                backend.delete_empty_key(&record.key)?;
            }
            if backend.key(&record.key)? != record.before {
                return Err(repair("Restored registry key presence/security differs"));
            }
        }
        for record in &self.shortcuts {
            let current = backend.shortcut(record.legacy)?;
            if !self.shortcut_admissible(record, &current) {
                return Err(repair("Shortcut changed immediately before restore"));
            }
            self.verify_unsealed_shortcut(backend, record, &current)?;
            if !files_equal(&current, &record.before) {
                backend.put_shortcut(record.legacy, &record.before)?;
            }
            backend.restore_directory(record.legacy, &record.directory_before)?;
            if !files_equal(&backend.shortcut(record.legacy)?, &record.before)
                || !directory_equal(
                    &backend.shortcut_directory(record.legacy)?,
                    &record.directory_before,
                )
            {
                return Err(repair(
                    "Restored shortcut bytes/permissions/presence differ",
                ));
            }
        }
        self.verify_before(backend)
    }
    fn verify_before(&self, backend: &mut impl Backend) -> Result<()> {
        self.verify_contents(backend)?;
        for record in &self.registry {
            if backend.value(&record.key, &record.name)? != record.before {
                return Err(repair("Registry restore readback changed"));
            }
        }
        for record in &self.keys {
            if backend.key(&record.key)? != record.before {
                return Err(repair("Registry key restore readback changed"));
            }
        }
        for record in &self.shortcuts {
            if !files_equal(&backend.shortcut(record.legacy)?, &record.before)
                || !directory_equal(
                    &backend.shortcut_directory(record.legacy)?,
                    &record.directory_before,
                )
                || backend.directory_other_entries(record.legacy)? != record.other_entries
            {
                return Err(repair("Shortcut restore readback changed"));
            }
        }
        Ok(())
    }
}
fn validate_paths(installation: &Path, directory: &Path) -> Result<()> {
    storage::plain_path(installation)?;
    storage::plain_path(directory)?;
    if directory.starts_with(installation) || installation.starts_with(directory) {
        return Err(security(
            "Integration recovery directory must be separate from installation",
        ));
    }
    // Fresh Setup snapshots before it creates {app}. Verify the closest existing
    // directory rather than forcing installation-directory creation early.
    for requested in [installation, directory] {
        let mut current = Some(requested);
        loop {
            let path = current
                .ok_or_else(|| security("No existing owner-verifiable directory ancestor"))?;
            match std::fs::symlink_metadata(path) {
                Ok(metadata) => {
                    if !metadata.is_dir() {
                        return Err(security(
                            "Installation/recovery ancestor is not a directory",
                        ));
                    }
                    platform::verify_current_user_path(path)?;
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => current = path.parent(),
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(())
}
fn valid_uninstall_command(installation: &Path, command: &str, quiet: bool) -> bool {
    let tail = if quiet { "\" /SILENT" } else { "\"" };
    let Some(inner) = command.strip_prefix('"').and_then(|v| v.strip_suffix(tail)) else {
        return false;
    };
    let path = Path::new(inner);
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let name = name.to_ascii_lowercase();
    path.parent().is_some_and(|p| {
        p.to_string_lossy()
            .eq_ignore_ascii_case(&installation.to_string_lossy())
    }) && name.len() == 12
        && name.starts_with("unins")
        && name[5..8].bytes().all(|b| b.is_ascii_digit())
        && name.ends_with(".exe")
}

/// Return only the uninstaller named by this installation's owned HKCU64 entry.
/// Similar-looking uninsNNN files elsewhere in the directory are not ours.
pub fn owned_uninstaller_files(installation: &Path) -> Result<Vec<String>> {
    storage::plain_path(installation)?;
    #[cfg(all(windows, not(test)))]
    {
        let mut native = native::Native::new()?;
        let key = uninstall_key();
        let Some(_) = native.key(&key)? else {
            return Ok(Vec::new());
        };
        let app = string_path(installation)?;
        if !native
            .value(&key, "Inno Setup: App Path")?
            .and_then(|v| v.text())
            .is_some_and(|s| s.eq_ignore_ascii_case(&app))
        {
            return Err(security(
                "Uninstaller registration belongs to a different installation",
            ));
        }
        let command = native
            .value(&key, "UninstallString")?
            .and_then(|v| v.text())
            .ok_or_else(|| security("Missing owned uninstall command"))?;
        if !valid_uninstall_command(installation, &command, false) {
            return Err(security(
                "Uninstall command points outside the owned installation",
            ));
        }
        let path = Path::new(&command[1..command.len() - 1]);
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| security("Invalid uninstaller name"))?;
        let mut files = Vec::new();
        for extension in ["exe", "dat", "msg"] {
            let name = format!("{stem}.{extension}");
            let path = installation.join(&name);
            storage::plain_path(&path)?;
            match std::fs::symlink_metadata(&path) {
                Ok(m) if m.is_file() => {
                    platform::verify_current_user_path(&path)?;
                    files.push(name);
                }
                Ok(_) => return Err(security("Owned uninstaller is not a regular file")),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && extension == "msg" => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(files)
    }
    #[cfg(any(not(windows), test))]
    {
        Ok(Vec::new())
    }
}

#[cfg(all(windows, not(test)))]
mod native {
    use super::*;
    use std::{
        ffi::{OsStr, c_void},
        fs,
        io::Read,
        os::windows::{
            ffi::{OsStrExt, OsStringExt},
            fs::OpenOptionsExt,
        },
        ptr,
    };
    use windows_sys::{
        Win32::{
            Foundation::*,
            Security::{Authorization::*, *},
            System::{Registry::*, Threading::*},
            UI::Shell::{FOLDERID_Programs, SHGetKnownFolderPath},
        },
        core::GUID,
    };
    const WRITE_DAC_ACCESS: u32 = 0x0004_0000;
    const MAX_NAME: usize = 256;
    const MAX_PATH_UNITS: usize = 32_768;
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn GetUserNameW(buffer: *mut u16, length: *mut u32) -> i32;
    }
    #[link(name = "ole32")]
    unsafe extern "system" {
        fn CoInitializeEx(reserved: *const c_void, flags: u32) -> i32;
        fn CoUninitialize();
        fn CoCreateInstance(
            class: *const GUID,
            outer: *mut c_void,
            context: u32,
            interface: *const GUID,
            object: *mut *mut c_void,
        ) -> i32;
        fn CoTaskMemFree(memory: *const c_void);
    }
    fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
        value.as_ref().encode_wide().chain(Some(0)).collect()
    }
    fn check(code: u32) -> Result<()> {
        if code != 0 {
            Err(std::io::Error::from_raw_os_error(code as i32).into())
        } else {
            Ok(())
        }
    }
    fn hresult(code: i32) -> Result<()> {
        if code < 0 {
            Err(repair(format!(
                "Windows shell-link inspection failed: {code:#x}"
            )))
        } else {
            Ok(())
        }
    }
    fn last_error() -> UpdateError {
        std::io::Error::last_os_error().into()
    }
    struct Local(*mut c_void);
    impl Drop for Local {
        fn drop(&mut self) {
            unsafe {
                LocalFree(self.0);
            }
        }
    }
    struct RegKey(HKEY);
    impl Drop for RegKey {
        fn drop(&mut self) {
            unsafe {
                RegCloseKey(self.0);
            }
        }
    }
    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
    struct CoMemory(*mut u16);
    impl Drop for CoMemory {
        fn drop(&mut self) {
            unsafe {
                CoTaskMemFree(self.0.cast());
            }
        }
    }
    pub(super) fn current_user_name() -> Result<String> {
        let mut buffer = [0u16; 257];
        let mut length = buffer.len() as u32;
        if unsafe { GetUserNameW(buffer.as_mut_ptr(), &mut length) } == 0 {
            return Err(last_error());
        }
        if length < 2 || length as usize > buffer.len() || buffer[length as usize - 1] != 0 {
            return Err(security("Invalid current-user name"));
        }
        String::from_utf16(&buffer[..length as usize - 1])
            .map_err(|_| security("Invalid current-user name encoding"))
    }
    pub(super) fn programs_directory() -> Result<PathBuf> {
        let mut path = ptr::null_mut();
        hresult(unsafe {
            SHGetKnownFolderPath(&FOLDERID_Programs, 0, ptr::null_mut(), &mut path)
        })?;
        if path.is_null() {
            return Err(security("Known folder returned a null path"));
        }
        let _memory = CoMemory(path);
        let mut len = 0;
        while len < MAX_PATH_UNITS && unsafe { *path.add(len) } != 0 {
            len += 1;
        }
        if len == MAX_PATH_UNITS {
            return Err(security("Start Menu path exceeds Windows bound"));
        }
        let path = PathBuf::from(std::ffi::OsString::from_wide(unsafe {
            std::slice::from_raw_parts(path, len)
        }));
        storage::plain_path(&path)?;
        platform::verify_current_user_path(&path)?;
        Ok(path)
    }
    pub(super) struct Native {
        programs: PathBuf,
        token: Vec<usize>,
    }
    impl Native {
        pub(super) fn new() -> Result<Self> {
            let mut token = ptr::null_mut();
            if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
                return Err(last_error());
            }
            let token_handle = Handle(token);
            let mut bytes = 0;
            unsafe {
                GetTokenInformation(token_handle.0, TokenUser, ptr::null_mut(), 0, &mut bytes)
            };
            if bytes == 0 || bytes > MAX_VALUE as u32 {
                return Err(security("Current-user token exceeds bound"));
            }
            let mut token = vec![0usize; (bytes as usize).div_ceil(std::mem::size_of::<usize>())];
            if unsafe {
                GetTokenInformation(
                    token_handle.0,
                    TokenUser,
                    token.as_mut_ptr().cast(),
                    bytes,
                    &mut bytes,
                )
            } == 0
            {
                return Err(last_error());
            }
            Ok(Self {
                programs: programs_directory()?,
                token,
            })
        }
        fn user_sid(&self) -> PSID {
            unsafe { (*self.token.as_ptr().cast::<TOKEN_USER>()).User.Sid }
        }
        fn path(&self, legacy: bool) -> PathBuf {
            if legacy {
                self.programs
                    .join("Light Image Tool")
                    .join("Light Image Tool.lnk")
            } else {
                self.programs
                    .join("gloamere-image")
                    .join("gloamere-image.lnk")
            }
        }
        // Traverse each component with OPEN_LINK, checking the link value before
        // using that key as a parent. Never follow a registry symbolic link.
        fn open(&self, path: &str, access: u32) -> Result<Option<RegKey>> {
            if !path.starts_with("Software\\") || path.contains(['\0', '/']) {
                return Err(security("Invalid registry path"));
            }
            let components: Vec<_> = path.split('\\').collect();
            if components
                .iter()
                .any(|s| s.is_empty() || *s == "." || *s == "..")
            {
                return Err(security("Invalid registry component"));
            }
            let mut owned: Option<RegKey> = None;
            for (index, component) in components.iter().enumerate() {
                let parent = owned.as_ref().map_or(HKEY_CURRENT_USER, |k| k.0);
                let mut handle = ptr::null_mut();
                let wanted = if index + 1 == components.len() {
                    access
                } else {
                    KEY_QUERY_VALUE | KEY_ENUMERATE_SUB_KEYS
                };
                let result = unsafe {
                    RegOpenKeyExW(
                        parent,
                        wide(component).as_ptr(),
                        REG_OPTION_OPEN_LINK,
                        wanted | KEY_QUERY_VALUE | KEY_WOW64_64KEY,
                        &mut handle,
                    )
                };
                if result == ERROR_FILE_NOT_FOUND || result == ERROR_PATH_NOT_FOUND {
                    return Ok(None);
                }
                check(result)?;
                let key = RegKey(handle);
                if Self::read_raw(key.0, "SymbolicLinkValue")?.is_some() {
                    return Err(security(
                        "Registry symbolic-link or ambiguous link marker refused",
                    ));
                }
                owned = Some(key);
            }
            Ok(owned)
        }
        fn read_raw(key: HKEY, name: &str) -> Result<Option<RawValue>> {
            let name = wide(name);
            let mut kind = 0;
            let mut length = 0;
            let code = unsafe {
                RegQueryValueExW(
                    key,
                    name.as_ptr(),
                    ptr::null(),
                    &mut kind,
                    ptr::null_mut(),
                    &mut length,
                )
            };
            if code == ERROR_FILE_NOT_FOUND {
                return Ok(None);
            }
            check(code)?;
            if length as usize > MAX_VALUE {
                return Err(security("Registry value exceeds bound"));
            }
            let mut bytes = vec![0u8; length as usize];
            let original_kind = kind;
            let original_length = length;
            check(unsafe {
                RegQueryValueExW(
                    key,
                    name.as_ptr(),
                    ptr::null(),
                    &mut kind,
                    bytes.as_mut_ptr(),
                    &mut length,
                )
            })?;
            if length != original_length || kind != original_kind {
                return Err(repair("Registry changed while snapshotting raw bytes"));
            }
            bytes.truncate(length as usize);
            Ok(Some(RawValue { kind, bytes }))
        }
        fn image(&self, key: HKEY) -> Result<KeyImage> {
            let mut descriptor = ptr::null_mut();
            let mut owner = ptr::null_mut();
            check(unsafe {
                GetSecurityInfo(
                    key.cast(),
                    SE_REGISTRY_KEY,
                    OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                    &mut owner,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    &mut descriptor,
                )
            })?;
            let _memory = Local(descriptor);
            if owner.is_null()
                || unsafe { IsValidSid(owner) } == 0
                || unsafe { EqualSid(owner, self.user_sid()) } == 0
            {
                return Err(security("Registry key is not owned by the current user"));
            }
            let mut text = ptr::null_mut();
            let mut length = 0;
            if unsafe {
                ConvertSecurityDescriptorToStringSecurityDescriptorW(
                    descriptor,
                    1,
                    OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                    &mut text,
                    &mut length,
                )
            } == 0
            {
                return Err(last_error());
            }
            let _text = Local(text.cast());
            if length == 0 || length > MAX_VALUE as u32 {
                return Err(security("Registry security descriptor exceeds bound"));
            }
            let sddl =
                String::from_utf16(unsafe { std::slice::from_raw_parts(text, length as usize) })
                    .map_err(|_| security("Invalid registry security descriptor encoding"))?
                    .trim_end_matches('\0')
                    .to_owned();
            Ok(KeyImage { sddl })
        }
        fn names(&self, key: HKEY, children: bool) -> Result<Vec<String>> {
            let limit = if children {
                8
            } else {
                UNINSTALL_NAMES.len() + 8
            };
            let mut result = Vec::new();
            for index in 0..=limit {
                let mut name = [0u16; MAX_NAME];
                let mut length = MAX_NAME as u32;
                let code = if children {
                    unsafe {
                        RegEnumKeyExW(
                            key,
                            index as u32,
                            name.as_mut_ptr(),
                            &mut length,
                            ptr::null(),
                            ptr::null_mut(),
                            ptr::null_mut(),
                            ptr::null_mut(),
                        )
                    }
                } else {
                    unsafe {
                        RegEnumValueW(
                            key,
                            index as u32,
                            name.as_mut_ptr(),
                            &mut length,
                            ptr::null(),
                            ptr::null_mut(),
                            ptr::null_mut(),
                            ptr::null_mut(),
                        )
                    }
                };
                if code == ERROR_NO_MORE_ITEMS {
                    return Ok(result);
                }
                check(code)?;
                if index == limit || length as usize >= MAX_NAME {
                    return Err(security("Unexpected product key cardinality/name size"));
                }
                result.push(
                    String::from_utf16(&name[..length as usize])
                        .map_err(|_| security("Invalid registry value/key name"))?,
                );
            }
            Err(security("Registry enumeration exceeds bound"))
        }
        fn directory(&self, legacy: bool) -> Result<PathBuf> {
            let path = self.path(legacy);
            let directory = path.parent().unwrap().to_owned();
            storage::plain_path(&directory)?;
            if directory.exists() {
                if !fs::metadata(&directory)?.is_dir() {
                    return Err(security("Product Start Menu group is not a directory"));
                }
                platform::verify_current_user_path(&directory)?;
            }
            Ok(directory)
        }
        fn companions(&self, legacy: bool) -> Result<()> {
            let path = self.path(legacy);
            for ext in ["pif", "url", ""] {
                let companion = path.with_extension(ext);
                storage::plain_path(&companion)?;
                match fs::symlink_metadata(&companion) {
                    Ok(_) => {
                        return Err(security("Conflicting product shortcut companion retained"));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
            Ok(())
        }
    }
    impl Backend for Native {
        fn user_name(&self) -> Result<String> {
            current_user_name()
        }
        fn programs(&self) -> &Path {
            &self.programs
        }
        fn value(&mut self, key: &str, name: &str) -> Result<Option<RawValue>> {
            let Some(key) = self.open(key, KEY_READ)? else {
                return Ok(None);
            };
            Self::read_raw(key.0, name)
        }
        fn key(&mut self, key: &str) -> Result<Option<KeyImage>> {
            let Some(key) = self.open(key, KEY_READ)? else {
                return Ok(None);
            };
            Ok(Some(self.image(key.0)?))
        }
        fn contents(&mut self, key: &str) -> Result<(Vec<String>, Vec<String>)> {
            let Some(key) = self.open(key, KEY_READ)? else {
                return Ok((Vec::new(), Vec::new()));
            };
            self.image(key.0)?;
            Ok((self.names(key.0, false)?, self.names(key.0, true)?))
        }
        fn create_key(&mut self, key: &str) -> Result<()> {
            if !fixed_keys().iter().any(|k| k == key) {
                return Err(security("Attempt to create non-product registry key"));
            }
            if self.open(key, KEY_READ)?.is_some() {
                return Err(repair(
                    "Registry key appeared immediately before recreation",
                ));
            }
            let (parent, name) = key
                .rsplit_once('\\')
                .ok_or_else(|| security("Missing registry parent"))?;
            let parent = self
                .open(parent, KEY_CREATE_SUB_KEY | KEY_READ)?
                .ok_or_else(|| repair("Shared registry parent vanished; do not recreate it"))?;
            let mut handle = ptr::null_mut();
            let mut disposition = 0;
            check(unsafe {
                RegCreateKeyExW(
                    parent.0,
                    wide(name).as_ptr(),
                    0,
                    ptr::null(),
                    REG_OPTION_NON_VOLATILE,
                    KEY_READ | KEY_SET_VALUE | WRITE_DAC_ACCESS | KEY_WOW64_64KEY,
                    ptr::null(),
                    &mut handle,
                    &mut disposition,
                )
            })?;
            let key = RegKey(handle);
            if disposition != REG_CREATED_NEW_KEY {
                return Err(repair("Registry key appeared during recreation"));
            }
            self.image(key.0)?;
            check(unsafe { RegFlushKey(key.0) })
        }
        fn set_value(&mut self, key: &str, name: &str, value: &Option<RawValue>) -> Result<()> {
            let Some(key) = self.open(key, KEY_READ | KEY_SET_VALUE)? else {
                return if value.is_none() {
                    Ok(())
                } else {
                    Err(repair("Registry key vanished before restoring value"))
                };
            };
            self.image(key.0)?;
            match value {
                Some(value) => {
                    value.validate()?;
                    check(unsafe {
                        RegSetValueExW(
                            key.0,
                            wide(name).as_ptr(),
                            0,
                            value.kind,
                            value.bytes.as_ptr(),
                            value.bytes.len() as u32,
                        )
                    })?;
                }
                None => {
                    let status = unsafe { RegDeleteValueW(key.0, wide(name).as_ptr()) };
                    if status != ERROR_FILE_NOT_FOUND {
                        check(status)?;
                    }
                }
            }
            check(unsafe { RegFlushKey(key.0) })
        }
        fn set_security(&mut self, key: &str, image: &KeyImage) -> Result<()> {
            if !fixed_keys().iter().any(|k| k == key) {
                return Err(security("Refusing shared registry key security mutation"));
            }
            let key = self
                .open(key, KEY_READ | WRITE_DAC_ACCESS)?
                .ok_or_else(|| repair("Key vanished before restoring ACL"))?;
            self.image(key.0)?;
            let mut descriptor = ptr::null_mut();
            if image.sddl.len() > MAX_VALUE
                || unsafe {
                    ConvertStringSecurityDescriptorToSecurityDescriptorW(
                        wide(&image.sddl).as_ptr(),
                        1,
                        &mut descriptor,
                        ptr::null_mut(),
                    )
                } == 0
            {
                return Err(security("Invalid saved registry security descriptor"));
            }
            let _memory = Local(descriptor);
            let mut owner = ptr::null_mut();
            let mut defaulted = 0;
            if unsafe { GetSecurityDescriptorOwner(descriptor, &mut owner, &mut defaulted) } == 0
                || owner.is_null()
                || unsafe { EqualSid(owner, self.user_sid()) } == 0
            {
                return Err(security("Saved registry ACL owner is not the current user"));
            }
            let mut control = 0;
            let mut revision = 0;
            if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0
            {
                return Err(last_error());
            }
            let protected = if control & SE_DACL_PROTECTED != 0 {
                PROTECTED_DACL_SECURITY_INFORMATION
            } else {
                UNPROTECTED_DACL_SECURITY_INFORMATION
            };
            check(unsafe {
                RegSetKeySecurity(key.0, DACL_SECURITY_INFORMATION | protected, descriptor)
            })?;
            check(unsafe { RegFlushKey(key.0) })?;
            if self.image(key.0)? != *image {
                return Err(repair("Restored registry owner/DACL readback differs"));
            }
            Ok(())
        }
        fn delete_empty_key(&mut self, key: &str) -> Result<()> {
            if !fixed_keys().iter().any(|k| k == key) {
                return Err(security("Refusing shared registry key removal"));
            }
            let Some(open) = self.open(key, KEY_READ)? else {
                return Ok(());
            };
            self.image(open.0)?;
            if !self.names(open.0, false)?.is_empty() || !self.names(open.0, true)?.is_empty() {
                return Err(repair("Nonempty product key retained"));
            }
            // RegDeleteKeyEx removes a single key only. No recursive delete exists
            // in this module. Keep the handle open until the deletion returns.
            check(unsafe {
                RegDeleteKeyExW(HKEY_CURRENT_USER, wide(key).as_ptr(), KEY_WOW64_64KEY, 0)
            })?;
            check(unsafe { RegFlushKey(HKEY_CURRENT_USER) })
        }
        fn shortcut(&mut self, legacy: bool) -> Result<Option<FileImage>> {
            self.companions(legacy)?;
            self.directory(legacy)?;
            let path = self.path(legacy);
            storage::plain_path(&path)?;
            let metadata = match fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e.into()),
            };
            if !metadata.is_file() || metadata.len() > MAX_SHORTCUT as u64 {
                return Err(security("Shortcut is not a bounded regular file"));
            }
            platform::verify_current_user_path(&path)?;
            // Disallow write/delete sharing while reading bytes and permissions.
            let file = fs::OpenOptions::new()
                .read(true)
                .share_mode(1)
                .open(&path)?;
            let permissions = platform::capture_permissions(&path)?;
            let mut bytes = Vec::new();
            file.take(MAX_SHORTCUT as u64 + 1).read_to_end(&mut bytes)?;
            if bytes.len() as u64 != metadata.len() || bytes.len() > MAX_SHORTCUT {
                return Err(repair("Shortcut changed while snapshotting"));
            }
            let image = FileImage::new(bytes, permissions);
            Ok(Some(image))
        }
        fn shortcut_owned(&mut self, legacy: bool, installation: &Path) -> Result<bool> {
            let path = self.path(legacy);
            storage::plain_path(&path)?;
            let Some(before) = self.shortcut(legacy)? else {
                return Ok(false);
            };
            let fields = read_shell_link(&path)?;
            let target = installation.join(if legacy {
                "light-image-tool.exe"
            } else {
                "gloamere-image.exe"
            });
            let owned = fields.target.eq_ignore_ascii_case(&string_path(&target)?)
                && fields.arguments.is_empty();
            // Verify the inspector read the same file we snapshotted, not a file
            // exchanged during COM loading. No Resolve, Save or launch is used.
            let after = self.shortcut(legacy)?;
            if !files_equal(&Some(before), &after) {
                return Err(repair("Shortcut changed during target inspection"));
            }
            Ok(owned)
        }
        fn directory_other_entries(&mut self, legacy: bool) -> Result<Vec<String>> {
            let directory = self.directory(legacy)?;
            if !directory.exists() {
                return Ok(Vec::new());
            }
            let shortcut = self.path(legacy);
            let expected = shortcut.file_name().unwrap().to_str().unwrap();
            let mut entries = Vec::new();
            for entry in fs::read_dir(&directory)? {
                let entry = entry?;
                let name = entry.file_name();
                let name = name
                    .to_str()
                    .ok_or_else(|| security("Invalid Start Menu sibling name"))?;
                if !name.eq_ignore_ascii_case(expected) {
                    entries.push(name.to_owned());
                }
                if entries.len() > 64 {
                    return Err(security("Too many files in product Start Menu group"));
                }
            }
            entries.sort();
            Ok(entries)
        }
        fn shortcut_directory(
            &mut self,
            legacy: bool,
        ) -> Result<Option<platform::FilePermissions>> {
            let path = self.directory(legacy)?;
            if path.exists() {
                Ok(Some(platform::capture_permissions(&path)?))
            } else {
                Ok(None)
            }
        }
        fn put_shortcut(&mut self, legacy: bool, image: &Option<FileImage>) -> Result<()> {
            self.companions(legacy)?;
            let directory = self.directory(legacy)?;
            let path = self.path(legacy);
            storage::plain_path(&path)?;
            match image {
                Some(image) => {
                    image.validate()?;
                    if !directory.exists() {
                        fs::create_dir(&directory)?;
                    }
                    platform::verify_current_user_path(&directory)?;
                    if path.exists() {
                        platform::verify_current_user_path(&path)?;
                    }
                    storage::atomic_bytes(&path, &image.bytes)?;
                    platform::restore_permissions(&path, &image.permissions)?;
                }
                None => {
                    if path.exists() {
                        platform::verify_current_user_path(&path)?;
                        fs::remove_file(&path)?;
                        storage::sync_directory(&directory)?;
                    }
                }
            }
            Ok(())
        }
        fn restore_directory(
            &mut self,
            legacy: bool,
            image: &Option<platform::FilePermissions>,
        ) -> Result<()> {
            let directory = self.directory(legacy)?;
            match image {
                Some(image) => {
                    if !directory.exists() {
                        fs::create_dir(&directory)?;
                    }
                    platform::restore_permissions(&directory, image)?;
                }
                None => {
                    if directory.exists() {
                        fs::remove_dir(&directory).map_err(|e|repair(format!("Product Start Menu directory is not empty or removable; retained: {e}")))?;
                        storage::sync_directory(&self.programs)?;
                    }
                }
            }
            Ok(())
        }
    }
    // Minimal ABI declarations keep shell-link inspection read-only without a
    // WSH/PowerShell subprocess or a second COM wrapper dependency.
    #[repr(C)]
    struct UnknownVTable {
        query: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32,
        add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
        release: unsafe extern "system" fn(*mut c_void) -> u32,
    }
    #[repr(C)]
    struct LinkVTable {
        unknown: UnknownVTable,
        get_path: unsafe extern "system" fn(*mut c_void, *mut u16, i32, *mut c_void, u32) -> i32,
        get_id_list: usize,
        set_id_list: usize,
        get_description: usize,
        set_description: usize,
        get_working_directory: usize,
        set_working_directory: usize,
        get_arguments: unsafe extern "system" fn(*mut c_void, *mut u16, i32) -> i32,
    }
    #[repr(C)]
    struct PersistVTable {
        unknown: UnknownVTable,
        get_class_id: usize,
        is_dirty: usize,
        load: unsafe extern "system" fn(*mut c_void, *const u16, u32) -> i32,
    }
    struct ComPtr(*mut c_void);
    impl Drop for ComPtr {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    let table = *self.0.cast::<*const UnknownVTable>();
                    ((*table).release)(self.0);
                }
            }
        }
    }
    struct Apartment(bool);
    impl Drop for Apartment {
        fn drop(&mut self) {
            if self.0 {
                unsafe {
                    CoUninitialize();
                }
            }
        }
    }
    struct LinkFields {
        target: String,
        arguments: String,
    }
    fn read_shell_link(path: &Path) -> Result<LinkFields> {
        // COINIT_APARTMENTTHREADED; an already initialized MTA can also load this
        // in-process object. RPC_E_CHANGED_MODE does not create a COM init count.
        let init = unsafe { CoInitializeEx(ptr::null(), 2) };
        if init < 0 && init != 0x80010106u32 as i32 {
            hresult(init)?;
        }
        let _apartment = Apartment(init >= 0);
        const SHELL_LINK: GUID = GUID::from_u128(0x00021401_0000_0000_c000_000000000046);
        const LINK_W: GUID = GUID::from_u128(0x000214f9_0000_0000_c000_000000000046);
        const PERSIST_FILE: GUID = GUID::from_u128(0x0000010b_0000_0000_c000_000000000046);
        let mut link = ptr::null_mut();
        hresult(unsafe { CoCreateInstance(&SHELL_LINK, ptr::null_mut(), 1, &LINK_W, &mut link) })?;
        if link.is_null() {
            return Err(security("Null IShellLink interface"));
        }
        let link = ComPtr(link);
        let table = unsafe { &**link.0.cast::<*const LinkVTable>() };
        let mut persist = ptr::null_mut();
        hresult(unsafe { (table.unknown.query)(link.0, &PERSIST_FILE, &mut persist) })?;
        if persist.is_null() {
            return Err(security("Null IPersistFile interface"));
        }
        let persist = ComPtr(persist);
        let persist_table = unsafe { &**persist.0.cast::<*const PersistVTable>() };
        hresult(unsafe { (persist_table.load)(persist.0, wide(path).as_ptr(), 0) })?; // STGM_READ
        let mut target = vec![0u16; MAX_PATH_UNITS];
        let mut arguments = vec![0u16; MAX_PATH_UNITS];
        // SLGP_RAWPATH avoids resolving or searching for a missing executable.
        hresult(unsafe {
            (table.get_path)(
                link.0,
                target.as_mut_ptr(),
                target.len() as i32,
                ptr::null_mut(),
                4,
            )
        })?;
        hresult(unsafe {
            (table.get_arguments)(link.0, arguments.as_mut_ptr(), arguments.len() as i32)
        })?;
        fn decode(value: &[u16]) -> Result<String> {
            let end = value
                .iter()
                .position(|v| *v == 0)
                .ok_or_else(|| security("Shell link string exceeds bound"))?;
            String::from_utf16(&value[..end]).map_err(|_| security("Invalid shell link Unicode"))
        }
        Ok(LinkFields {
            target: decode(&target)?,
            arguments: decode(&arguments)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Clone)]
    struct MemoryBackend {
        programs: PathBuf,
        values: BTreeMap<(String, String), RawValue>,
        keys: BTreeMap<String, KeyImage>,
        files: [Option<FileImage>; 2],
        directories: [Option<platform::FilePermissions>; 2],
        siblings: [Vec<String>; 2],
        owned: [bool; 2],
        writes: usize,
    }
    fn permissions() -> platform::FilePermissions {
        platform::FilePermissions {
            readonly: false,
            unix_mode: None,
            windows_sddl: Some("O:current-userD:P(A;;FA;;;current-user)".into()),
        }
    }
    fn key_image() -> KeyImage {
        KeyImage {
            sddl: "O:current-userD:P(A;;KA;;;current-user)".into(),
        }
    }
    impl MemoryBackend {
        fn initial(programs: PathBuf, installation: &Path) -> Self {
            let mut this = Self {
                programs,
                values: BTreeMap::new(),
                keys: BTreeMap::new(),
                files: [
                    Some(FileImage::new(
                        b"old-current-shortcut".to_vec(),
                        permissions(),
                    )),
                    Some(FileImage::new(
                        b"old-legacy-shortcut".to_vec(),
                        permissions(),
                    )),
                ],
                directories: [Some(permissions()), Some(permissions())],
                siblings: [Vec::new(), Vec::new()],
                owned: [true, true],
                writes: 0,
            };
            this.install(installation, true, "0.6.0");
            // `install` represents successful new Setup, so reintroduce the old
            // migration shortcut to exercise its exact byte/ACL restoration.
            this.files[1] = Some(FileImage::new(
                b"old-legacy-shortcut".to_vec(),
                permissions(),
            ));
            this.directories[1] = Some(permissions());
            this.writes = 0;
            this
        }
        fn install(&mut self, installation: &Path, explorer: bool, version: &str) {
            let expected = expected_values(installation, explorer, version).unwrap();
            for ((key, name), (dynamic, expected)) in expected {
                let value = if dynamic {
                    Some(match name.as_str() {
                        "UninstallString" => RawValue::sz(&format!(
                            "\"{}\"",
                            installation.join("unins000.exe").display()
                        )),
                        "QuietUninstallString" => RawValue::sz(&format!(
                            "\"{}\" /SILENT",
                            installation.join("unins000.exe").display()
                        )),
                        "Inno Setup: Setup Version" => RawValue::sz("6.7.1"),
                        "Inno Setup: User" => RawValue::sz("test-user"),
                        "Inno Setup: Language" => RawValue::sz("english"),
                        "InstallDate" => RawValue::sz("20261010"),
                        "EstimatedSize" => RawValue::dword(123456),
                        other => panic!("Unexpected fixture dynamic value {other}"),
                    })
                } else {
                    expected
                };
                match value {
                    Some(v) => {
                        self.values.insert((key, name), v);
                    }
                    None => {
                        self.values.remove(&(key, name));
                    }
                }
            }
            for key in fixed_keys() {
                if self
                    .values
                    .keys()
                    .any(|(k, _)| k == &key || k.starts_with(&format!(r"{key}\")))
                {
                    self.keys.insert(key, key_image());
                } else {
                    self.keys.remove(&key);
                }
            }
            self.keys.insert(DISCOVERY.into(), key_image());
            self.files[0] = Some(FileImage::new(
                format!("shortcut-{version}").into_bytes(),
                permissions(),
            ));
            self.directories[0] = Some(permissions());
            self.files[1] = None;
            if self.siblings[1].is_empty() {
                self.directories[1] = None;
            }
        }
    }
    impl Backend for MemoryBackend {
        fn user_name(&self) -> Result<String> {
            Ok("test-user".into())
        }
        fn programs(&self) -> &Path {
            &self.programs
        }
        fn value(&mut self, key: &str, name: &str) -> Result<Option<RawValue>> {
            Ok(self.values.get(&(key.into(), name.into())).cloned())
        }
        fn key(&mut self, key: &str) -> Result<Option<KeyImage>> {
            Ok(self.keys.get(key).cloned())
        }
        fn contents(&mut self, key: &str) -> Result<(Vec<String>, Vec<String>)> {
            let names = self
                .values
                .keys()
                .filter(|(k, _)| k == key)
                .map(|(_, n)| n.clone())
                .collect();
            let prefix = format!(r"{key}\");
            let children = self
                .keys
                .keys()
                .filter_map(|k| k.strip_prefix(&prefix))
                .filter(|s| !s.contains('\\'))
                .map(str::to_owned)
                .collect();
            Ok((names, children))
        }
        fn create_key(&mut self, key: &str) -> Result<()> {
            self.writes += 1;
            if self.keys.insert(key.into(), key_image()).is_some() {
                return Err(repair("Memory key already exists"));
            }
            Ok(())
        }
        fn set_value(&mut self, key: &str, name: &str, value: &Option<RawValue>) -> Result<()> {
            self.writes += 1;
            if value.is_some() && !self.keys.contains_key(key) {
                return Err(repair("Memory key missing"));
            }
            if let Some(value) = value {
                self.values.insert((key.into(), name.into()), value.clone());
            } else {
                self.values.remove(&(key.into(), name.into()));
            }
            Ok(())
        }
        fn set_security(&mut self, key: &str, image: &KeyImage) -> Result<()> {
            self.writes += 1;
            *self
                .keys
                .get_mut(key)
                .ok_or_else(|| repair("Memory key missing"))? = image.clone();
            Ok(())
        }
        fn delete_empty_key(&mut self, key: &str) -> Result<()> {
            let (values, children) = self.contents(key)?;
            if !values.is_empty() || !children.is_empty() {
                return Err(repair("Memory key not empty"));
            }
            self.writes += 1;
            self.keys.remove(key);
            Ok(())
        }
        fn shortcut(&mut self, legacy: bool) -> Result<Option<FileImage>> {
            Ok(self.files[legacy as usize].clone())
        }
        fn shortcut_owned(&mut self, legacy: bool, _: &Path) -> Result<bool> {
            Ok(self.owned[legacy as usize])
        }
        fn directory_other_entries(&mut self, legacy: bool) -> Result<Vec<String>> {
            Ok(self.siblings[legacy as usize].clone())
        }
        fn shortcut_directory(
            &mut self,
            legacy: bool,
        ) -> Result<Option<platform::FilePermissions>> {
            Ok(self.directories[legacy as usize].clone())
        }
        fn put_shortcut(&mut self, legacy: bool, image: &Option<FileImage>) -> Result<()> {
            self.writes += 1;
            self.files[legacy as usize] = image.clone();
            Ok(())
        }
        fn restore_directory(
            &mut self,
            legacy: bool,
            image: &Option<platform::FilePermissions>,
        ) -> Result<()> {
            let index = legacy as usize;
            if image.is_none() && (!self.siblings[index].is_empty() || self.files[index].is_some())
            {
                return Err(repair("Memory directory not empty"));
            }
            if !directory_equal(&self.directories[index], image) {
                self.writes += 1;
                self.directories[index] = image.clone();
            }
            Ok(())
        }
    }
    fn fixture() -> (tempfile::TempDir, PathBuf, MemoryBackend) {
        let directory = tempfile::tempdir().unwrap();
        let installation = directory.path().join("install");
        let memory = MemoryBackend::initial(directory.path().join("programs"), &installation);
        (directory, installation, memory)
    }
    fn begin(memory: &mut MemoryBackend, installation: &Path, explorer: bool) -> Snapshot {
        let mut snapshot = Snapshot::capture_with(memory, installation, explorer, "0.7.0").unwrap();
        snapshot.mark_write_intent().unwrap();
        snapshot
    }
    #[test]
    fn exact_raw_types_bytes_shortcuts_acl_and_absence_roundtrip() {
        let (_tmp, app, mut memory) = fixture();
        let old = RawValue {
            kind: 2,
            bytes: vec![0, 1, 2, 0, 255, 0],
        };
        memory
            .values
            .insert((uninstall_key(), "DisplayIcon".into()), old.clone());
        memory
            .values
            .remove(&(uninstall_key(), "VersionMinor".into()));
        memory.files[0].as_mut().unwrap().permissions.readonly = true;
        let initial = memory.clone();
        let mut snapshot = begin(&mut memory, &app, false);
        let serialized = serde_json::to_vec(&snapshot).unwrap();
        snapshot = serde_json::from_slice(&serialized).unwrap();
        memory.install(&app, false, "0.7.0");
        snapshot.seal_with(&mut memory).unwrap();
        let snapshot: Snapshot =
            serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
        snapshot.restore_with(&mut memory).unwrap();
        assert_eq!(memory.values, initial.values);
        assert_eq!(memory.keys, initial.keys);
        assert!(files_equal(&memory.files[0], &initial.files[0]));
        assert!(files_equal(&memory.files[1], &initial.files[1]));
        let writes = memory.writes;
        snapshot.restore_with(&mut memory).unwrap();
        assert_eq!(memory.writes, writes);
    }
    #[test]
    fn newly_created_product_keys_removed_but_shared_values_preserved() {
        let (_tmp, app, mut memory) = fixture();
        for key in [menu_key(".gif"), format!(r"{}\command", menu_key(".gif"))] {
            memory.keys.remove(&key);
            memory.values.retain(|(k, _), _| k != &key);
        }
        memory.values.insert(
            (DISCOVERY.into(), "another app".into()),
            RawValue::sz("foreign-handler"),
        );
        let before = memory.clone();
        let mut snapshot = begin(&mut memory, &app, true);
        memory.install(&app, true, "0.7.0");
        snapshot.seal_with(&mut memory).unwrap();
        snapshot.restore_with(&mut memory).unwrap();
        assert_eq!(memory.keys, before.keys);
        assert_eq!(memory.values, before.values);
    }
    #[test]
    fn foreign_value_blocks_whole_restore_before_any_mutation() {
        let (_tmp, app, mut memory) = fixture();
        let mut snapshot = begin(&mut memory, &app, true);
        memory.install(&app, true, "0.7.0");
        snapshot.seal_with(&mut memory).unwrap();
        memory
            .values
            .insert((PROG.into(), "foreign-added".into()), RawValue::sz("keep"));
        let before = memory.values.clone();
        assert!(snapshot.restore_with(&mut memory).is_err());
        assert_eq!(memory.writes, 0);
        assert_eq!(memory.values, before);
    }
    #[test]
    fn foreign_changed_known_value_and_acl_are_retained_after_sealing() {
        let (_tmp, app, mut memory) = fixture();
        let mut snapshot = begin(&mut memory, &app, true);
        memory.install(&app, true, "0.7.0");
        snapshot.seal_with(&mut memory).unwrap();
        memory.values.insert(
            (PROG.into(), "OwnerId".into()),
            RawValue::sz("another product"),
        );
        assert!(snapshot.restore_with(&mut memory).is_err());
        assert_eq!(memory.writes, 0);
        memory
            .values
            .insert((PROG.into(), "OwnerId".into()), RawValue::sz(PRODUCT_ID));
        memory
            .keys
            .get_mut(PROG)
            .unwrap()
            .sddl
            .push_str("foreign-acl");
        assert!(snapshot.restore_with(&mut memory).is_err());
        assert_eq!(memory.writes, 0);
    }
    #[test]
    fn same_target_different_shortcut_bytes_are_not_adopted_at_rollback() {
        let (_tmp, app, mut memory) = fixture();
        let mut snapshot = begin(&mut memory, &app, true);
        memory.install(&app, true, "0.7.0");
        snapshot.seal_with(&mut memory).unwrap();
        let foreign = Some(FileImage::new(
            b"same-target-but-user-modified".to_vec(),
            permissions(),
        ));
        memory.files[0] = foreign.clone();
        memory.owned[0] = true;
        assert!(snapshot.restore_with(&mut memory).is_err());
        assert_eq!(memory.writes, 0);
        assert!(files_equal(&memory.files[0], &foreign));
    }
    #[test]
    fn unknown_names_children_and_foreign_owners_block_capture() {
        let (_tmp, app, mut memory) = fixture();
        let original = memory.clone();
        memory.values.insert(
            (uninstall_key(), "unexpected vendor data".into()),
            RawValue::sz("keep"),
        );
        assert!(Snapshot::capture_with(&mut memory, &app, true, "0.7.0").is_err());
        memory = original.clone();
        memory.keys.insert(format!(r"{PROG}\foreign"), key_image());
        assert!(Snapshot::capture_with(&mut memory, &app, true, "0.7.0").is_err());
        memory = original;
        memory.values.insert(
            (ROOT.into(), "OwnerPath".into()),
            RawValue::sz("C:\\foreign.exe"),
        );
        assert!(Snapshot::capture_with(&mut memory, &app, true, "0.7.0").is_err());
        assert_eq!(memory.writes, 0);
    }
    #[test]
    fn unsealed_dynamic_torn_write_requires_persisted_begin_intent() {
        let (_tmp, app, mut memory) = fixture();
        let mut snapshot = Snapshot::capture_with(&mut memory, &app, true, "0.7.0").unwrap();
        let before = memory.values.clone();
        memory.values.insert(
            (uninstall_key(), "EstimatedSize".into()),
            RawValue {
                kind: 4,
                bytes: vec![1, 2],
            },
        );
        assert!(snapshot.restore_with(&mut memory).is_err());
        assert_eq!(memory.writes, 0);
        snapshot.mark_write_intent().unwrap();
        snapshot.restore_with(&mut memory).unwrap();
        assert_eq!(memory.values, before);
    }
    #[test]
    fn interrupted_uninstall_key_deletion_restores_old_owned_key() {
        let (_tmp, app, mut memory) = fixture();
        let before = memory.clone();
        let snapshot = begin(&mut memory, &app, true);
        memory.keys.remove(&uninstall_key());
        memory.values.retain(|(k, _), _| k != &uninstall_key());
        snapshot.restore_with(&mut memory).unwrap();
        assert_eq!(memory.values, before.values);
        assert_eq!(memory.keys, before.keys);
    }
    #[test]
    fn new_unsealed_shortcut_preserves_evidence() {
        let (_tmp, app, mut memory) = fixture();
        memory.files[0] = None;
        memory.directories[0] = None;
        let snapshot = begin(&mut memory, &app, true);
        memory.files[0] = Some(FileImage::new(
            b"installer-or-concurrent-file".to_vec(),
            permissions(),
        ));
        memory.directories[0] = Some(permissions());
        assert!(snapshot.restore_with(&mut memory).is_err());
        assert_eq!(memory.writes, 0);
    }
    #[test]
    fn foreign_start_menu_sibling_blocks_before_registry_restore() {
        let (_tmp, app, mut memory) = fixture();
        let mut snapshot = begin(&mut memory, &app, true);
        memory.install(&app, true, "0.7.0");
        snapshot.seal_with(&mut memory).unwrap();
        memory.siblings[0].push("personal note.txt".into());
        assert!(snapshot.restore_with(&mut memory).is_err());
        assert_eq!(memory.writes, 0);
    }
    #[test]
    fn sealing_checks_expected_product_values_and_is_idempotent() {
        let (_tmp, app, mut memory) = fixture();
        let mut snapshot = begin(&mut memory, &app, true);
        memory.install(&app, true, "0.7.0");
        memory.values.insert(
            (uninstall_key(), "UninstallString".into()),
            RawValue::sz("\"C:\\foreign\\unins000.exe\""),
        );
        assert!(snapshot.seal_with(&mut memory).is_err());
        assert!(!snapshot.sealed);
        memory.install(&app, true, "0.7.0");
        snapshot.seal_with(&mut memory).unwrap();
        snapshot.seal_with(&mut memory).unwrap();
        assert_eq!(memory.writes, 0);
    }
    #[test]
    fn journal_rejects_unknown_duplicate_missing_and_redirected_targets() {
        let (_tmp, app, mut memory) = fixture();
        let snapshot = begin(&mut memory, &app, true);
        let mut malformed = snapshot.clone();
        malformed.registry[0].key = r"Software\Classes\.jpg\UserChoice".into();
        assert!(malformed.validate_structure(&app).is_err());
        let mut malformed = snapshot.clone();
        malformed.registry[1] = malformed.registry[0].clone();
        assert!(malformed.validate_structure(&app).is_err());
        let mut malformed = snapshot.clone();
        malformed.keys.pop();
        assert!(malformed.validate_structure(&app).is_err());
        let mut malformed = snapshot.clone();
        malformed.registry[0].expected = Some(RawValue::sz("foreign"));
        assert!(malformed.validate_structure(&app).is_err());
        let mut json = serde_json::to_value(&snapshot).unwrap();
        json["unknown"] = true.into();
        assert!(serde_json::from_value::<Snapshot>(json).is_err());
        let mut malformed = snapshot;
        malformed.shortcuts[0].before.as_mut().unwrap().bytes[0] ^= 1;
        assert!(malformed.validate_structure(&app).is_err());
    }
    #[test]
    fn raw_empty_reg_none_differs_from_absence_and_preserves_unusual_bytes() {
        assert_ne!(
            Some(RawValue {
                kind: 0,
                bytes: Vec::new()
            }),
            None
        );
        assert!(
            RawValue {
                kind: 1,
                bytes: vec![0x61, 0, 0, 0, 0x62, 0, 0, 0]
            }
            .text()
            .is_none()
        );
        assert_eq!(RawValue::sz("中文路径").text().as_deref(), Some("中文路径"));
        let (_tmp, app, mut memory) = fixture();
        let unusual = RawValue {
            kind: 0,
            bytes: Vec::new(),
        };
        memory
            .values
            .insert((uninstall_key(), "DisplayIcon".into()), unusual.clone());
        let mut snapshot = begin(&mut memory, &app, true);
        memory.install(&app, true, "0.7.0");
        snapshot.seal_with(&mut memory).unwrap();
        snapshot.restore_with(&mut memory).unwrap();
        assert_eq!(
            memory.value(&uninstall_key(), "DisplayIcon").unwrap(),
            Some(unusual)
        );
    }
    #[test]
    fn uninstall_commands_are_fixed_in_installation_without_added_arguments() {
        let (_tmp, app, _) = fixture();
        let path = app.join("unins007.exe");
        assert!(valid_uninstall_command(
            &app,
            &format!("\"{}\"", path.display()),
            false
        ));
        assert!(valid_uninstall_command(
            &app,
            &format!("\"{}\" /SILENT", path.display()),
            true
        ));
        for command in [
            format!("\"{}\" /anything", path.display()),
            format!("\"{}\"", app.join("other.exe").display()),
            format!("\"{}\"", app.join("unins007.exe/../evil.exe").display()),
        ] {
            assert!(!valid_uninstall_command(&app, &command, false));
        }
    }
    #[test]
    fn old_owned_torn_shortcut_requires_intent_and_restores_exactly() {
        let (_tmp, app, mut memory) = fixture();
        let mut snapshot = Snapshot::capture_with(&mut memory, &app, true, "0.7.0").unwrap();
        let before = memory.files[0].clone();
        memory.files[0] = Some(FileImage::new(Vec::new(), permissions()));
        assert!(snapshot.restore_with(&mut memory).is_err());
        assert_eq!(memory.writes, 0);
        snapshot.mark_write_intent().unwrap();
        snapshot.restore_with(&mut memory).unwrap();
        assert!(files_equal(&memory.files[0], &before));
    }
    #[test]
    fn complete_retargeted_old_shortcut_is_preserved_even_after_begin() {
        let (_tmp, app, mut memory) = fixture();
        let snapshot = begin(&mut memory, &app, true);
        let mut bytes = vec![0; 76];
        bytes[..4].copy_from_slice(&[76, 0, 0, 0]);
        bytes[4..20].copy_from_slice(&[1, 20, 2, 0, 0, 0, 0, 0, 192, 0, 0, 0, 0, 0, 0, 70]);
        memory.files[0] = Some(FileImage::new(bytes, permissions()));
        memory.owned[0] = false;
        assert!(snapshot.restore_with(&mut memory).is_err());
        assert_eq!(memory.writes, 0);
    }
    #[test]
    fn native_legacy_migration_is_exact_idempotent_and_recoverable() {
        let (_tmp, app, mut memory) = fixture();
        let before = memory.files[1].clone();
        let mut snapshot = Snapshot::capture_with(&mut memory, &app, true, "0.7.0").unwrap();
        assert!(snapshot.remove_legacy_with(&mut memory).is_err());
        assert_eq!(memory.writes, 0);
        snapshot.mark_write_intent().unwrap();
        snapshot.remove_legacy_with(&mut memory).unwrap();
        assert!(memory.files[1].is_none() && memory.directories[1].is_none());
        snapshot.remove_legacy_with(&mut memory).unwrap();
        snapshot.restore_with(&mut memory).unwrap();
        assert!(files_equal(&memory.files[1], &before));
    }
    #[test]
    fn native_legacy_migration_preserves_foreign_changes_and_other_group_files() {
        let (_tmp, app, mut memory) = fixture();
        memory.siblings[1] = vec!["personal note.txt".into()];
        let snapshot = begin(&mut memory, &app, true);
        let before = memory.files[1].clone();
        memory.files[1] = Some(FileImage::new(
            b"foreign-changed-link".to_vec(),
            permissions(),
        ));
        assert!(snapshot.remove_legacy_with(&mut memory).is_err());
        assert_eq!(memory.writes, 0);
        memory.files[1] = before;
        snapshot.remove_legacy_with(&mut memory).unwrap();
        assert!(memory.files[1].is_none() && memory.directories[1].is_some());
        assert_eq!(memory.siblings[1], vec!["personal note.txt"]);
    }
    #[test]
    fn clean_install_seals_current_user_without_old_uninstall_entry() {
        let (_tmp, app, mut memory) = fixture();
        memory.values.clear();
        memory.keys.clear();
        memory.files = [None, None];
        memory.directories = [None, None];
        let mut snapshot = begin(&mut memory, &app, true);
        memory.install(&app, true, "0.7.0");
        snapshot.seal_with(&mut memory).unwrap();
        snapshot.restore_with(&mut memory).unwrap();
        assert!(memory.values.is_empty());
        assert!(fixed_keys().iter().all(|k| !memory.keys.contains_key(k)));
        assert!(memory.files.iter().all(Option::is_none));
    }
    #[test]
    fn raw_binary_preimage_is_not_retyped_on_restore() {
        let (_tmp, app, mut memory) = fixture();
        let binary = RawValue {
            kind: 3,
            bytes: vec![0, 255, 127, 1, 0],
        };
        memory
            .values
            .insert((uninstall_key(), "DisplayIcon".into()), binary.clone());
        let mut snapshot = begin(&mut memory, &app, true);
        memory.install(&app, true, "0.7.0");
        snapshot.seal_with(&mut memory).unwrap();
        snapshot.restore_with(&mut memory).unwrap();
        assert_eq!(
            memory.value(&uninstall_key(), "DisplayIcon").unwrap(),
            Some(binary)
        );
    }
    #[cfg(any(not(windows), test))]
    #[test]
    fn explicit_portable_fixture_never_contains_native_state() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("app");
        std::fs::create_dir(&app).unwrap();
        let journal = dir.path().join("transaction");
        let mut snapshot = Snapshot::capture(&app, &journal, true, "0.7.0").unwrap();
        assert!(snapshot.fixture && snapshot.registry.is_empty() && snapshot.shortcuts.is_empty());
        snapshot.mark_write_intent().unwrap();
        snapshot.seal_after(&app, &journal).unwrap();
        snapshot.restore(&app, &journal).unwrap();
        assert!(owned_uninstaller_files(&app).unwrap().is_empty());
        let absent = dir.path().join("not-yet-created").join("app");
        let missing_snapshot = Snapshot::capture(&absent, &journal, true, "0.7.0").unwrap();
        missing_snapshot.validate(&absent, &journal).unwrap();
        assert!(!absent.exists());
    }
}
