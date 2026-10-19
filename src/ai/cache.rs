//! Bounded SQLite evidence cache, with independently retained manual decisions.
//! Every disk path is supplied by the caller. No image or sidecar is ever opened.
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::limits::Limit;
use rusqlite::{Connection, OpenFlags, Transaction, params};
use thiserror::Error;

use super::types::{AssetIdentity, MachineRecord, ManualOverride, is_content_hash};

pub const SCHEMA_VERSION: u32 = 2;
pub const DEFAULT_AUTOMATIC_BUDGET: u64 = 100 * 1024 * 1024;
pub const MAX_RECORD_BYTES: usize = 64 * 1024;
pub const MAX_KEY_BYTES: usize = 4096;
pub const MAX_EVIDENCE_ITEMS: usize = 64;
const MAX_SUMMARY_BYTES: usize = 8192;
const MAX_PROVIDER_BYTES: usize = 1024;
const ROW_ACCOUNTING_OVERHEAD: usize = 128;
const MAX_SQL_VALUE_BYTES: usize = MAX_RECORD_BYTES + MAX_KEY_BYTES + 1024;
const SQL_EXECUTION_BUDGET: Duration = Duration::from_millis(25);
const SQL_PROGRESS_INTERVAL: i32 = 1000;
const MAX_SQL_PROGRESS_CALLBACKS: u32 = 500;
// "GLAI": prevents accidentally treating unrelated SQLite files as our cache.
const APPLICATION_ID: u32 = 0x474c_4149;

#[derive(Debug, Error)]
pub enum CacheError {
    #[error("AI cache I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("AI cache SQLite error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("AI cache record error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("Invalid AI cache record: {0}")]
    InvalidRecord(&'static str),
    #[error("AI cache record is {size} bytes; limit is {limit}")]
    RecordTooLarge { size: usize, limit: usize },
    #[error(
        "AI cache schema {found} is unsupported (current {supported}); original file preserved"
    )]
    UnsupportedSchema { found: u32, supported: u32 },
    #[error("The selected file is not a recognized AI cache; original file preserved")]
    UnrecognizedDatabase,
    #[error("AI cache integrity check failed: {0}; original file preserved")]
    Integrity(String),
    #[error("No persistent AI cache path was supplied")]
    MissingPath,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheStats {
    pub persistent: bool,
    pub automatic_records: u64,
    /// Accounted automatic bytes (payload, keys and per-row overhead). SQLite
    /// pages/indexes and independent manual records are not an exact disk cap.
    pub automatic_bytes: u64,
    pub manual_records: u64,
}

pub struct Cache {
    connection: Connection,
    path: Option<PathBuf>,
    persistent: bool,
    automatic_budget: u64,
    clock: i64,
    /// While automatic persistence is off, manual records stay in this store.
    manual_disk: Option<Connection>,
}

impl Cache {
    pub fn open(path: &Path, persistent: bool) -> Result<Self, CacheError> {
        Self::open_with_budget(path, persistent, DEFAULT_AUTOMATIC_BUDGET)
    }

    pub fn open_with_budget(
        path: &Path,
        persistent: bool,
        automatic_budget: u64,
    ) -> Result<Self, CacheError> {
        if !persistent {
            let mut cache = Self::open_with_budget(path, true, automatic_budget)?;
            cache.set_persistent(false)?;
            return Ok(cache);
        }
        // Probe existing files read-only BEFORE any pragmas, migrations or writes.
        // Corrupt, foreign and future databases are not renamed/deleted/replaced.
        let existing_version = inspect_existing(path)?;
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )?;
        configure(&connection)?;
        if existing_version.is_none() {
            connection.pragma_update(None, "auto_vacuum", "FULL")?;
            create_schema(&connection)?;
        } else if existing_version == Some(1) {
            migrate_v1(&connection)?;
        }
        let clock = connection.query_row(
            "SELECT coalesce(max(touched), 0) FROM automatic_results",
            [],
            |row| row.get(0),
        )?;
        let mut cache = Self {
            connection,
            path: Some(path.to_owned()),
            persistent: true,
            automatic_budget,
            clock,
            manual_disk: None,
        };
        cache.enforce_budget()?;
        Ok(cache)
    }

    pub fn memory() -> Result<Self, CacheError> {
        Self::memory_with_budget(DEFAULT_AUTOMATIC_BUDGET)
    }

    pub fn memory_with_budget(automatic_budget: u64) -> Result<Self, CacheError> {
        let connection = Connection::open_in_memory()?;
        configure(&connection)?;
        create_schema(&connection)?;
        Ok(Self {
            connection,
            path: None,
            persistent: false,
            automatic_budget,
            clock: 0,
            manual_disk: None,
        })
    }

    pub fn is_persistent(&self) -> bool {
        self.persistent
    }

    /// The caller MUST re-hash the current file before lookup. Matching path,
    /// length and mtime are never enough to retrieve a previous result.
    pub fn get_machine(
        &mut self,
        identity: &AssetIdentity,
    ) -> Result<Option<MachineRecord>, CacheError> {
        validate_identity(identity)?;
        reset_sql_budget(&self.connection);
        let value = read_payload(
            &self.connection,
            "SELECT length(payload), CASE WHEN length(payload) <= ?3 THEN payload ELSE NULL END
             FROM automatic_results WHERE asset_key = ?1 AND content_hash = ?2",
            &identity.key,
            Some(&identity.content_hash),
        )?;
        let Some(payload) = value else {
            return Ok(None);
        };
        let mut record: MachineRecord = serde_json::from_slice(&payload)?;
        validate_machine(&record)?;
        if !record.identity.same_content(identity) {
            return Err(CacheError::InvalidRecord(
                "stored identity does not match its key",
            ));
        }
        // Cached/imported approval is not authorization. The scheduler may
        // rebind this ONLY to the current shipped package allowlist.
        if let Some(model) = &mut record.model {
            model.approved = false;
        }
        // Non-authoritative metadata may change after a rename/timestamp edit.
        record.identity = identity.clone();
        let touched = self.tick();
        self.connection.execute(
            "UPDATE automatic_results SET touched = ?3 WHERE asset_key = ?1 AND content_hash = ?2",
            params![identity.key, identity.content_hash, touched],
        )?;
        Ok(Some(record))
    }

    pub fn put_machine(&mut self, record: &MachineRecord) -> Result<(), CacheError> {
        validate_machine(record)?;
        reset_sql_budget(&self.connection);
        let payload = bounded_serialize(record)?;
        let cost = accounted_size(
            &record.identity.key,
            &record.identity.content_hash,
            &payload,
        );
        let touched = self.tick();
        let tx = self.connection.transaction()?;
        tx.execute(
            "INSERT INTO automatic_results(asset_key,content_hash,payload,byte_len,touched)
             VALUES(?1,?2,?3,?4,?5) ON CONFLICT(asset_key,content_hash) DO UPDATE SET
             payload=excluded.payload,byte_len=excluded.byte_len,touched=excluded.touched",
            params![
                record.identity.key,
                record.identity.content_hash,
                payload,
                cost as i64,
                touched
            ],
        )?;
        prune(&tx, self.automatic_budget)?;
        tx.commit()?;
        Ok(())
    }

    /// Returns an old override too: the policy decides whether the verified
    /// current hash matches. Stale decisions remain visible for reconfirmation.
    pub fn get_manual(&self, asset_key: &str) -> Result<Option<ManualOverride>, CacheError> {
        validate_key(asset_key)?;
        reset_sql_budget(self.manual_connection());
        let payload = read_payload(
            self.manual_connection(),
            "SELECT length(payload), CASE WHEN length(payload) <= ?3 THEN payload ELSE NULL END
             FROM manual_overrides WHERE asset_key = ?1 AND (?2 IS NULL)",
            asset_key,
            None,
        )?;
        let Some(payload) = payload else {
            return Ok(None);
        };
        let manual: ManualOverride = serde_json::from_slice(&payload)?;
        validate_manual(&manual)?;
        let hash_matches: bool = self.manual_connection().query_row(
            "SELECT content_hash = ?2 FROM manual_overrides WHERE asset_key = ?1",
            params![asset_key, manual.content_hash],
            |row| row.get(0),
        )?;
        if !hash_matches {
            return Err(CacheError::InvalidRecord(
                "stored manual hash does not match its key",
            ));
        }
        Ok(Some(manual))
    }

    pub fn set_manual(
        &mut self,
        asset_key: &str,
        manual: &ManualOverride,
    ) -> Result<(), CacheError> {
        validate_key(asset_key)?;
        validate_manual(manual)?;
        reset_sql_budget(self.manual_connection());
        let payload = bounded_serialize(manual)?;
        self.manual_connection().execute(
            "INSERT INTO manual_overrides(asset_key,content_hash,payload) VALUES(?1,?2,?3)
             ON CONFLICT(asset_key) DO UPDATE SET content_hash=excluded.content_hash,payload=excluded.payload",
            params![asset_key, manual.content_hash, payload],
        )?;
        Ok(())
    }

    pub fn remove_manual(&mut self, asset_key: &str) -> Result<(), CacheError> {
        validate_key(asset_key)?;
        reset_sql_budget(self.manual_connection());
        self.manual_connection().execute(
            "DELETE FROM manual_overrides WHERE asset_key = ?1",
            [asset_key],
        )?;
        Ok(())
    }

    pub fn clear_automatic(&mut self) -> Result<(), CacheError> {
        reset_sql_budget(&self.connection);
        reset_sql_budget(self.manual_connection());
        if let Some(disk) = &self.manual_disk {
            disk.execute("DELETE FROM automatic_results", [])?;
        }
        self.connection
            .execute("DELETE FROM automatic_results", [])?;
        Ok(())
    }

    pub fn clear_manual(&mut self) -> Result<(), CacheError> {
        reset_sql_budget(self.manual_connection());
        self.manual_connection()
            .execute("DELETE FROM manual_overrides", [])?;
        Ok(())
    }

    /// Disabling automatic persistence moves automatic results to memory.
    /// Manual decisions remain durable in the supplied database. Explicit clear
    /// operations affect both disk and memory. Re-enabling merges session results.
    /// A failed transition leaves the live cache available and unchanged.
    pub fn set_persistent(&mut self, persistent: bool) -> Result<(), CacheError> {
        if persistent == self.persistent {
            return Ok(());
        }
        let mut destination = if persistent {
            let path = self.path.as_ref().ok_or(CacheError::MissingPath)?;
            Self::open_with_budget(path, true, self.automatic_budget)?
        } else {
            let mut destination = Self::memory_with_budget(self.automatic_budget)?;
            destination.path = self.path.clone();
            destination
        };
        destination.merge_from(self)?;
        let old = std::mem::replace(self, destination);
        if !persistent {
            self.manual_disk = Some(old.connection);
        }
        Ok(())
    }

    pub fn stats(&self) -> Result<CacheStats, CacheError> {
        reset_sql_budget(&self.connection);
        reset_sql_budget(self.manual_connection());
        let (automatic_records, automatic_bytes) = self.connection.query_row(
            "SELECT count(*),coalesce(sum(byte_len),0) FROM automatic_results",
            [],
            |r| Ok((r.get::<_, u64>(0)?, r.get::<_, u64>(1)?)),
        )?;
        let manual_records = self.manual_connection().query_row(
            "SELECT count(*) FROM manual_overrides",
            [],
            |r| r.get(0),
        )?;
        Ok(CacheStats {
            persistent: self.persistent,
            automatic_records,
            automatic_bytes,
            manual_records,
        })
    }

    fn manual_connection(&self) -> &Connection {
        self.manual_disk.as_ref().unwrap_or(&self.connection)
    }

    fn tick(&mut self) -> i64 {
        self.clock = self.clock.saturating_add(1);
        self.clock
    }

    fn enforce_budget(&mut self) -> Result<(), CacheError> {
        reset_sql_budget(&self.connection);
        let tx = self.connection.transaction()?;
        prune(&tx, self.automatic_budget)?;
        tx.commit()?;
        Ok(())
    }

    fn merge_from(&mut self, source: &Cache) -> Result<(), CacheError> {
        reset_sql_budget(&self.connection);
        reset_sql_budget(&source.connection);
        reset_sql_budget(source.manual_connection());
        let tx = self.connection.transaction()?;
        let mut automatic = source.connection.prepare(
            "SELECT CASE WHEN length(CAST(asset_key AS BLOB))<=?2 THEN asset_key ELSE NULL END,
                    CASE WHEN length(CAST(content_hash AS BLOB))=64 THEN content_hash ELSE NULL END,
                    length(payload),CASE WHEN length(payload)<=?1 THEN payload ELSE NULL END
             FROM automatic_results ORDER BY touched ASC,rowid ASC",
        )?;
        let mut rows = automatic.query(params![MAX_RECORD_BYTES as i64, MAX_KEY_BYTES as i64])?;
        let mut touched = self.clock;
        while let Some(row) = rows.next()? {
            let key: String = row.get(0)?;
            let hash: String = row.get(1)?;
            let payload = row_payload(row, 2, 3)?;
            let record: MachineRecord = serde_json::from_slice(&payload)?;
            validate_machine(&record)?;
            if record.identity.key != key || record.identity.content_hash != hash {
                return Err(CacheError::InvalidRecord(
                    "stored machine identity mismatch",
                ));
            }
            touched = touched.saturating_add(1);
            tx.execute(
                "INSERT INTO automatic_results(asset_key,content_hash,payload,byte_len,touched)
                 VALUES(?1,?2,?3,?4,?5) ON CONFLICT(asset_key,content_hash) DO UPDATE SET
                 payload=excluded.payload,byte_len=excluded.byte_len,touched=excluded.touched",
                params![
                    key,
                    hash,
                    payload,
                    accounted_size(&key, &hash, &payload) as i64,
                    touched
                ],
            )?;
        }
        let mut manuals = source.manual_connection().prepare(
            "SELECT CASE WHEN length(CAST(asset_key AS BLOB))<=?2 THEN asset_key ELSE NULL END,
                    CASE WHEN length(CAST(content_hash AS BLOB))=64 THEN content_hash ELSE NULL END,
                    length(payload),CASE WHEN length(payload)<=?1 THEN payload ELSE NULL END
             FROM manual_overrides",
        )?;
        let mut rows = manuals.query(params![MAX_RECORD_BYTES as i64, MAX_KEY_BYTES as i64])?;
        while let Some(row) = rows.next()? {
            let key: String = row.get(0)?;
            let hash: String = row.get(1)?;
            let payload = row_payload(row, 2, 3)?;
            let manual: ManualOverride = serde_json::from_slice(&payload)?;
            validate_key(&key)?;
            validate_manual(&manual)?;
            if manual.content_hash != hash {
                return Err(CacheError::InvalidRecord("stored manual identity mismatch"));
            }
            tx.execute(
                "INSERT INTO manual_overrides(asset_key,content_hash,payload) VALUES(?1,?2,?3)
                 ON CONFLICT(asset_key) DO UPDATE SET content_hash=excluded.content_hash,payload=excluded.payload",
                params![key, hash, payload],
            )?;
        }
        prune(&tx, self.automatic_budget)?;
        tx.commit()?;
        self.clock = touched;
        Ok(())
    }
}

fn configure(connection: &Connection) -> Result<(), CacheError> {
    configure_limits(connection)?;
    connection.pragma_update(None, "trusted_schema", "OFF")?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    connection.pragma_update(None, "secure_delete", "ON")?;
    Ok(())
}

fn configure_limits(connection: &Connection) -> Result<(), CacheError> {
    connection.busy_timeout(Duration::ZERO)?;
    connection.set_limit(Limit::SQLITE_LIMIT_LENGTH, MAX_SQL_VALUE_BYTES as i32)?;
    connection.set_limit(Limit::SQLITE_LIMIT_SQL_LENGTH, 16 * 1024)?;
    connection.set_limit(Limit::SQLITE_LIMIT_COLUMN, 32)?;
    connection.set_limit(Limit::SQLITE_LIMIT_EXPR_DEPTH, 32)?;
    connection.set_limit(Limit::SQLITE_LIMIT_COMPOUND_SELECT, 8)?;
    connection.set_limit(Limit::SQLITE_LIMIT_VARIABLE_NUMBER, 16)?;
    connection.set_limit(Limit::SQLITE_LIMIT_TRIGGER_DEPTH, 0)?;
    connection.set_limit(Limit::SQLITE_LIMIT_ATTACHED, 0)?;
    connection.set_limit(Limit::SQLITE_LIMIT_WORKER_THREADS, 0)?;
    reset_sql_budget(connection);
    Ok(())
}

/// SQLite invokes this between VM instructions, so this bounds SQL computation,
/// not a blocked filesystem call. Each public operation gets a fresh budget.
fn reset_sql_budget(connection: &Connection) {
    let deadline = Instant::now() + SQL_EXECUTION_BUDGET;
    let mut callbacks = 0_u32;
    connection.progress_handler(
        SQL_PROGRESS_INTERVAL,
        Some(move || {
            callbacks += 1;
            callbacks >= MAX_SQL_PROGRESS_CALLBACKS || Instant::now() >= deadline
        }),
    );
}

fn inspect_existing(path: &Path) -> Result<Option<u32>, CacheError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.file_type().is_file() {
        return Err(CacheError::UnrecognizedDatabase);
    }
    if metadata.len() == 0 {
        return Ok(None);
    }
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    configure_limits(&connection)?;
    let application_id: u32 =
        connection.pragma_query_value(None, "application_id", |r| r.get(0))?;
    if application_id != APPLICATION_ID {
        return Err(CacheError::UnrecognizedDatabase);
    }
    let version: u32 = connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if !(1..=SCHEMA_VERSION).contains(&version) {
        return Err(CacheError::UnsupportedSchema {
            found: version,
            supported: SCHEMA_VERSION,
        });
    }
    let integrity: String = connection.query_row("PRAGMA quick_check(1)", [], |r| r.get(0))?;
    if integrity != "ok" {
        return Err(CacheError::Integrity(integrity));
    }
    // Verify a known shape before permitting a migration or normal mutations.
    connection
        .prepare("SELECT asset_key,content_hash,payload,touched FROM automatic_results LIMIT 0")?;
    connection.prepare("SELECT asset_key,content_hash,payload FROM manual_overrides LIMIT 0")?;
    if version == 2 {
        connection.prepare("SELECT byte_len FROM automatic_results LIMIT 0")?;
    }
    Ok(Some(version))
}

fn create_schema(connection: &Connection) -> Result<(), CacheError> {
    connection.execute_batch(
        "BEGIN IMMEDIATE;
         CREATE TABLE automatic_results (
             asset_key TEXT NOT NULL,
             content_hash TEXT NOT NULL,
             payload BLOB NOT NULL,
             byte_len INTEGER NOT NULL CHECK(byte_len >= 0),
             touched INTEGER NOT NULL,
             PRIMARY KEY(asset_key,content_hash)
         );
         CREATE INDEX automatic_lru ON automatic_results(touched);
         CREATE TABLE manual_overrides (
             asset_key TEXT PRIMARY KEY NOT NULL,
             content_hash TEXT NOT NULL,
             payload BLOB NOT NULL
         );",
    )?;
    if let Err(error) = (|| -> Result<(), rusqlite::Error> {
        connection.pragma_update(None, "application_id", APPLICATION_ID)?;
        connection.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        connection.execute_batch("COMMIT;")?;
        Ok(())
    })() {
        let _ = connection.execute_batch("ROLLBACK;");
        return Err(error.into());
    }
    Ok(())
}

fn migrate_v1(connection: &Connection) -> Result<(), CacheError> {
    let tx = connection.unchecked_transaction()?;
    tx.execute_batch(
        "ALTER TABLE automatic_results ADD COLUMN byte_len INTEGER NOT NULL DEFAULT 0 CHECK(byte_len >= 0);
         CREATE INDEX IF NOT EXISTS automatic_lru ON automatic_results(touched);",
    )?;
    tx.execute(
        "UPDATE automatic_results SET byte_len=length(payload)+length(CAST(asset_key AS BLOB))+length(CAST(content_hash AS BLOB))+?1",
        [ROW_ACCOUNTING_OVERHEAD as i64],
    )?;
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    tx.commit()?;
    Ok(())
}

fn prune(tx: &Transaction<'_>, budget: u64) -> Result<(), CacheError> {
    // Keep the newest prefix that fits; never touch manual_overrides.
    tx.execute(
        "DELETE FROM automatic_results WHERE rowid IN (
          SELECT rowid FROM (
            SELECT rowid,SUM(byte_len) OVER (ORDER BY touched DESC,rowid DESC) AS running
            FROM automatic_results
          ) WHERE running > ?1
        )",
        [budget.min(i64::MAX as u64) as i64],
    )?;
    Ok(())
}

fn validate_key(key: &str) -> Result<(), CacheError> {
    if key.is_empty() || key.len() > MAX_KEY_BYTES || key.contains('\0') {
        return Err(CacheError::InvalidRecord(
            "empty, oversized or NUL-containing stable asset key",
        ));
    }
    Ok(())
}

fn validate_identity(identity: &AssetIdentity) -> Result<(), CacheError> {
    validate_key(&identity.key)?;
    if !identity.has_content_hash() {
        return Err(CacheError::InvalidRecord(
            "a full verified SHA-256 is required",
        ));
    }
    Ok(())
}

fn validate_manual(manual: &ManualOverride) -> Result<(), CacheError> {
    if !is_content_hash(&manual.content_hash) {
        return Err(CacheError::InvalidRecord(
            "manual decision requires a full SHA-256",
        ));
    }
    Ok(())
}

fn validate_machine(record: &MachineRecord) -> Result<(), CacheError> {
    validate_identity(&record.identity)?;
    if record.evidence.len() > MAX_EVIDENCE_ITEMS {
        return Err(CacheError::InvalidRecord("too many evidence items"));
    }
    for evidence in &record.evidence {
        if evidence.provider.len() > MAX_PROVIDER_BYTES
            || evidence.summary.len() > MAX_SUMMARY_BYTES
        {
            return Err(CacheError::InvalidRecord("oversized evidence text"));
        }
    }
    if record.model.as_ref().is_some_and(|m| !m.valid()) {
        return Err(CacheError::InvalidRecord("invalid model output"));
    }
    Ok(())
}

fn bounded_serialize<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, CacheError> {
    // Stop serialization at the limit rather than allocating an unbounded Vec
    // and checking its size only afterwards.
    struct BoundedBytes {
        bytes: Vec<u8>,
        oversized: Option<usize>,
    }
    impl std::io::Write for BoundedBytes {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            let next = self.bytes.len().saturating_add(buffer.len());
            if next > MAX_RECORD_BYTES {
                self.oversized = Some(next);
                return Err(std::io::Error::other("AI cache record size limit"));
            }
            self.bytes.extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut output = BoundedBytes {
        bytes: Vec::new(),
        oversized: None,
    };
    let result = serde_json::to_writer(&mut output, value);
    if let Some(size) = output.oversized {
        return Err(CacheError::RecordTooLarge {
            size,
            limit: MAX_RECORD_BYTES,
        });
    }
    result?;
    Ok(output.bytes)
}

fn check_size(size: usize) -> Result<(), CacheError> {
    if size > MAX_RECORD_BYTES {
        Err(CacheError::RecordTooLarge {
            size,
            limit: MAX_RECORD_BYTES,
        })
    } else {
        Ok(())
    }
}

fn accounted_size(key: &str, hash: &str, payload: &[u8]) -> usize {
    key.len() + hash.len() + payload.len() + ROW_ACCOUNTING_OVERHEAD
}

fn read_payload(
    connection: &Connection,
    query: &str,
    key: &str,
    hash: Option<&str>,
) -> Result<Option<Vec<u8>>, CacheError> {
    let mut statement = connection.prepare(query)?;
    let mut rows = statement.query(params![key, hash, MAX_RECORD_BYTES as i64])?;
    rows.next()?.map(|row| row_payload(row, 0, 1)).transpose()
}

fn row_payload(
    row: &rusqlite::Row<'_>,
    length_index: usize,
    payload_index: usize,
) -> Result<Vec<u8>, CacheError> {
    let size: usize = row.get(length_index)?;
    check_size(size)?;
    Ok(row.get(payload_index)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::policy::decide;
    use crate::ai::types::{Bucket, Evidence, EvidenceKind, ManualDecision, ModelResult, Phase};
    use tempfile::tempdir;

    fn record(key: &str, hash_digit: char) -> MachineRecord {
        MachineRecord {
            identity: AssetIdentity {
                key: key.into(),
                content_hash: hash_digit.to_string().repeat(64),
                size: 42,
                modified_ns: 17,
            },
            evidence: vec![Evidence {
                kind: EvidenceKind::UnsignedGeneration,
                provider: "test".into(),
                summary: "generation workflow".into(),
                authenticated: false,
            }],
            phase: Phase::AwaitingModel,
            model: None,
            checked_unix_seconds: 1,
            component_state: Default::default(),
            run_state: None,
            release_tier: Default::default(),
            source_error: None,
            pixel_checked_unix_seconds: None,
            pack_revision: None,
        }
    }

    fn manual(hash_digit: char) -> ManualOverride {
        ManualOverride {
            content_hash: hash_digit.to_string().repeat(64),
            decision: ManualDecision::NotAi,
            created_unix_seconds: 1,
        }
    }

    #[test]
    fn persistent_round_trip_and_new_content_never_inherits_old_results() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("ai.sqlite");
        let old = record("fs:one", 'a');
        {
            let mut cache = Cache::open(&path, true).unwrap();
            cache.put_machine(&old).unwrap();
            cache.set_manual(&old.identity.key, &manual('a')).unwrap();
        }
        let mut cache = Cache::open(&path, true).unwrap();
        assert_eq!(cache.get_machine(&old.identity).unwrap(), Some(old.clone()));
        let changed = record("fs:one", 'b');
        assert!(cache.get_machine(&changed.identity).unwrap().is_none());
        let stale = cache.get_manual("fs:one").unwrap().unwrap();
        assert_eq!(stale, manual('a'));
        let mut pending = MachineRecord::unscanned(changed.identity.clone());
        let d = decide(Some(&pending), Some(&stale));
        assert_eq!(d.bucket, Bucket::Pending);
        assert!(!d.manual_active);
        cache.put_machine(&changed).unwrap();
        // Old content may remain as an evictable cache version, never selected by a new hash.
        assert!(cache.get_machine(&old.identity).unwrap().is_some());
        pending.identity.key = "fs:replacement".into();
        assert!(cache.get_machine(&pending.identity).unwrap().is_none());
    }

    #[test]
    fn unchanged_full_hash_reuses_stable_identity_after_rename_or_timestamp_change() {
        let mut cache = Cache::memory().unwrap();
        let r = record("fs:volume:inode", 'a');
        cache.put_machine(&r).unwrap();
        let mut refreshed = r.identity.clone();
        refreshed.modified_ns = 999;
        let result = cache.get_machine(&refreshed).unwrap().unwrap();
        assert_eq!(result.identity, refreshed);
        assert_eq!(result.evidence, r.evidence);
        let mut different_file = refreshed;
        different_file.key = "fs:other_inode".into();
        assert!(cache.get_machine(&different_file).unwrap().is_none());
    }

    #[test]
    fn lru_budget_never_evicts_manual_decisions_and_clear_operations_are_separate() {
        let a = record("fs:a", 'a');
        let b = record("fs:b", 'b');
        let c = record("fs:c", 'c');
        let cost = accounted_size(
            &a.identity.key,
            &a.identity.content_hash,
            &serde_json::to_vec(&a).unwrap(),
        ) as u64;
        let mut cache = Cache::memory_with_budget(cost * 2).unwrap();
        cache.set_manual(&b.identity.key, &manual('b')).unwrap();
        cache.put_machine(&a).unwrap();
        cache.put_machine(&b).unwrap();
        cache.get_machine(&a.identity).unwrap(); // Touch A, so B is oldest.
        cache.put_machine(&c).unwrap();
        assert!(cache.get_machine(&b.identity).unwrap().is_none());
        assert!(cache.get_machine(&a.identity).unwrap().is_some());
        assert!(cache.get_machine(&c.identity).unwrap().is_some());
        assert!(cache.get_manual(&b.identity.key).unwrap().is_some());
        let stats = cache.stats().unwrap();
        assert_eq!(stats.automatic_records, 2);
        assert!(stats.automatic_bytes <= cost * 2);
        assert_eq!(stats.manual_records, 1);
        cache.clear_automatic().unwrap();
        assert_eq!(cache.stats().unwrap().automatic_records, 0);
        assert!(cache.get_manual(&b.identity.key).unwrap().is_some());
        cache.put_machine(&a).unwrap();
        cache.clear_manual().unwrap();
        assert_eq!(cache.stats().unwrap().manual_records, 0);
        assert!(cache.get_machine(&a.identity).unwrap().is_some());
    }

    #[test]
    fn zero_automatic_budget_keeps_manuals() {
        let mut cache = Cache::memory_with_budget(0).unwrap();
        let r = record("fs:a", 'a');
        cache.put_machine(&r).unwrap();
        cache.set_manual("fs:a", &manual('a')).unwrap();
        assert_eq!(cache.stats().unwrap().automatic_records, 0);
        assert_eq!(cache.stats().unwrap().manual_records, 1);
    }

    #[test]
    fn persistence_off_keeps_automatic_results_in_memory_but_manuals_durable() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("ai.sqlite");
        let r = record("fs:a", 'a');
        let mut cache = Cache::open(&path, false).unwrap();
        cache.put_machine(&r).unwrap();
        cache.set_manual("fs:a", &manual('a')).unwrap();
        assert!(cache.get_machine(&r.identity).unwrap().is_some());
        drop(cache);
        let mut reopened = Cache::open(&path, true).unwrap();
        assert!(reopened.get_machine(&r.identity).unwrap().is_none());
        assert!(reopened.get_manual("fs:a").unwrap().is_some());
        assert!(!Cache::memory().unwrap().is_persistent());
    }

    #[test]
    fn toggling_persistence_keeps_current_session_and_remembers_explicit_deletions() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("ai.sqlite");
        let a = record("fs:a", 'a');
        let b = record("fs:b", 'b');
        let mut cache = Cache::open(&path, true).unwrap();
        cache.put_machine(&a).unwrap();
        cache.set_manual("fs:a", &manual('a')).unwrap();
        cache.set_manual("fs:b", &manual('b')).unwrap();
        cache.set_persistent(false).unwrap();
        cache.put_machine(&b).unwrap();
        cache.remove_manual("fs:a").unwrap();
        let mut observer = Cache::open(&path, true).unwrap();
        assert!(observer.get_machine(&b.identity).unwrap().is_none());
        assert!(observer.get_manual("fs:a").unwrap().is_none());
        drop(observer);
        cache.set_persistent(true).unwrap();
        assert!(cache.get_machine(&a.identity).unwrap().is_some());
        assert!(cache.get_machine(&b.identity).unwrap().is_some());
        assert!(cache.get_manual("fs:a").unwrap().is_none());
        assert!(cache.get_manual("fs:b").unwrap().is_some());
        cache.set_persistent(false).unwrap();
        cache.clear_automatic().unwrap();
        cache.clear_manual().unwrap();
        let observer = Cache::open(&path, true).unwrap();
        assert_eq!(observer.stats().unwrap().automatic_records, 0);
        assert_eq!(observer.stats().unwrap().manual_records, 0);
        drop(observer);
        cache.set_persistent(true).unwrap();
        assert_eq!(cache.stats().unwrap().automatic_records, 0);
        assert_eq!(cache.stats().unwrap().manual_records, 0);
    }

    #[test]
    fn new_offline_session_does_not_erase_old_manuals_when_enabled() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("ai.sqlite");
        let mut original = Cache::open(&path, true).unwrap();
        original.set_manual("fs:a", &manual('a')).unwrap();
        drop(original);
        let mut session = Cache::open(&path, false).unwrap();
        session.set_manual("fs:b", &manual('b')).unwrap();
        session.set_persistent(true).unwrap();
        assert!(session.get_manual("fs:a").unwrap().is_some());
        assert!(session.get_manual("fs:b").unwrap().is_some());
    }

    #[test]
    fn corrupt_foreign_and_future_databases_are_preserved_byte_for_byte() {
        let dir = tempdir().unwrap();
        let corrupt = dir.path().join("corrupt.sqlite");
        std::fs::write(&corrupt, b"this is not a SQLite database").unwrap();
        let foreign = dir.path().join("foreign.sqlite");
        let foreign_conn = Connection::open(&foreign).unwrap();
        foreign_conn
            .execute("CREATE TABLE private_data(secret TEXT)", [])
            .unwrap();
        drop(foreign_conn);
        let future = dir.path().join("future.sqlite");
        drop(Cache::open(&future, true).unwrap());
        let future_conn = Connection::open(&future).unwrap();
        future_conn
            .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        drop(future_conn);
        for path in [&corrupt, &foreign, &future] {
            let before = std::fs::read(path).unwrap();
            assert!(Cache::open(path, true).is_err());
            assert_eq!(std::fs::read(path).unwrap(), before);
        }
        assert!(matches!(
            Cache::open(&future, true),
            Err(CacheError::UnsupportedSchema { .. })
        ));
        assert!(matches!(
            Cache::open(&foreign, true),
            Err(CacheError::UnrecognizedDatabase)
        ));
    }

    #[test]
    fn v1_migration_preserves_manuals_and_adds_lru_accounting() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("v1.sqlite");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE automatic_results(asset_key TEXT NOT NULL,content_hash TEXT NOT NULL,payload BLOB NOT NULL,touched INTEGER NOT NULL,PRIMARY KEY(asset_key,content_hash)); CREATE TABLE manual_overrides(asset_key TEXT PRIMARY KEY NOT NULL,content_hash TEXT NOT NULL,payload BLOB NOT NULL);").unwrap();
        conn.pragma_update(None, "application_id", APPLICATION_ID)
            .unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        let r = record("fs:a", 'a');
        conn.execute(
            "INSERT INTO automatic_results VALUES(?1,?2,?3,1)",
            params![
                r.identity.key,
                r.identity.content_hash,
                serde_json::to_vec(&r).unwrap()
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO manual_overrides VALUES(?1,?2,?3)",
            params![
                r.identity.key,
                r.identity.content_hash,
                serde_json::to_vec(&manual('a')).unwrap()
            ],
        )
        .unwrap();
        drop(conn);
        let mut cache = Cache::open(&path, true).unwrap();
        assert_eq!(cache.get_machine(&r.identity).unwrap(), Some(r));
        assert_eq!(cache.get_manual("fs:a").unwrap(), Some(manual('a')));
        assert!(cache.stats().unwrap().automatic_bytes > 0);
        let version: u32 = cache
            .connection
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn malformed_known_schema_is_rejected_without_mutation() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("incomplete.sqlite");
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "application_id", APPLICATION_ID)
            .unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        conn.execute("CREATE TABLE unrelated(value TEXT)", [])
            .unwrap();
        drop(conn);
        let before = std::fs::read(&path).unwrap();
        assert!(Cache::open(&path, true).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn oversized_and_invalid_records_are_bounded_and_do_not_poison_existing_data() {
        let mut cache = Cache::memory().unwrap();
        let good = record("fs:a", 'a');
        cache.put_machine(&good).unwrap();
        let mut bad = good.clone();
        bad.evidence[0].summary = "x".repeat(MAX_SUMMARY_BYTES + 1);
        assert!(cache.put_machine(&bad).is_err());
        bad = good.clone();
        bad.evidence = vec![bad.evidence[0].clone(); MAX_EVIDENCE_ITEMS + 1];
        assert!(cache.put_machine(&bad).is_err());
        bad = good.clone();
        bad.phase = Phase::Failed("x".repeat(MAX_RECORD_BYTES));
        assert!(matches!(
            cache.put_machine(&bad),
            Err(CacheError::RecordTooLarge { .. })
        ));
        bad = good.clone();
        bad.identity.content_hash.clear();
        assert!(cache.put_machine(&bad).is_err());
        assert!(cache.get_machine(&bad.identity).is_err());
        assert!(
            cache
                .set_manual(
                    "fs:a",
                    &ManualOverride {
                        content_hash: String::new(),
                        ..manual('a')
                    }
                )
                .is_err()
        );
        assert_eq!(cache.get_machine(&good.identity).unwrap(), Some(good));
    }

    #[test]
    fn corrupt_or_oversized_payload_is_not_loaded_as_a_classification() {
        let mut cache = Cache::memory().unwrap();
        let r = record("fs:a", 'a');
        cache.put_machine(&r).unwrap();
        cache
            .connection
            .execute(
                "UPDATE automatic_results SET payload=?1",
                [b"not json".as_slice()],
            )
            .unwrap();
        assert!(cache.get_machine(&r.identity).is_err());
        cache
            .connection
            .execute(
                "UPDATE automatic_results SET payload=zeroblob(?1)",
                [(MAX_RECORD_BYTES + 1) as i64],
            )
            .unwrap();
        assert!(matches!(
            cache.get_machine(&r.identity),
            Err(CacheError::RecordTooLarge { .. })
        ));
        cache.set_manual("fs:a", &manual('a')).unwrap();
        cache
            .connection
            .execute(
                "UPDATE manual_overrides SET content_hash=?1",
                ["b".repeat(64)],
            )
            .unwrap();
        assert!(cache.get_manual("fs:a").is_err());
    }

    #[test]
    fn cached_approval_never_authorizes_a_model() {
        let mut cache = Cache::memory().unwrap();
        let mut r = record("fs:a", 'a');
        r.evidence.clear();
        r.phase = Phase::Complete;
        r.model = Some(ModelResult {
            fingerprint: "forged:approved".into(),
            input_scope: "full".into(),
            raw_score: 0.99,
            threshold: 0.5,
            approved: true,
            high_signal: true,
        });
        cache.put_machine(&r).unwrap();
        let loaded = cache.get_machine(&r.identity).unwrap().unwrap();
        assert!(!loaded.model.as_ref().unwrap().approved);
        assert_eq!(decide(Some(&loaded), None).bucket, Bucket::Pending);
    }

    #[test]
    fn missing_persistent_path_is_an_explicit_recoverable_error() {
        let mut cache = Cache::memory().unwrap();
        assert!(matches!(
            cache.set_persistent(true),
            Err(CacheError::MissingPath)
        ));
        assert!(!cache.is_persistent());
    }

    #[test]
    fn does_not_touch_source_pixels_metadata_or_sidecars() {
        let dir = tempdir().unwrap();
        let image = dir.path().join("original.png");
        let contents = b"opaque pixel and metadata bytes";
        std::fs::write(&image, contents).unwrap();
        let mut cache = Cache::open(&dir.path().join("ai.sqlite"), true).unwrap();
        let r = record(image.to_str().unwrap(), 'a');
        cache.put_machine(&r).unwrap();
        cache.set_manual(&r.identity.key, &manual('a')).unwrap();
        cache.clear_automatic().unwrap();
        cache.clear_manual().unwrap();
        assert_eq!(std::fs::read(&image).unwrap(), contents);
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 2);
    }
    #[test]
    fn creates_only_the_explicit_cache_parent_directory() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("app-data").join("ai").join("cache.sqlite");
        let cache = Cache::open(&path, false).unwrap();
        assert!(path.is_file());
        assert!(!cache.is_persistent());
        assert_eq!(cache.stats().unwrap().manual_records, 0);
    }

    #[test]
    fn malformed_oversized_keys_and_hashes_cannot_escape_copy_bounds() {
        for column in ["asset_key", "content_hash"] {
            let mut cache = Cache::memory().unwrap();
            let r = record("fs:a", 'a');
            cache.put_machine(&r).unwrap();
            cache
                .connection
                .execute(
                    &format!("UPDATE automatic_results SET {column}=?1"),
                    ["x".repeat(MAX_KEY_BYTES + 1)],
                )
                .unwrap();
            let mut destination = Cache::memory().unwrap();
            assert!(destination.merge_from(&cache).is_err());
            assert_eq!(destination.stats().unwrap().automatic_records, 0);
        }
        let mut cache = Cache::memory().unwrap();
        cache.set_manual("fs:a", &manual('a')).unwrap();
        cache
            .connection
            .execute(
                "UPDATE manual_overrides SET content_hash=?1",
                ["x".repeat(MAX_KEY_BYTES + 1)],
            )
            .unwrap();
        assert!(cache.get_manual("fs:a").is_err());
        let mut destination = Cache::memory().unwrap();
        assert!(destination.merge_from(&cache).is_err());
    }

    #[test]
    fn sqlite_limits_reject_large_values_before_materializing_them() {
        let cache = Cache::memory().unwrap();
        assert_eq!(
            cache.connection.limit(Limit::SQLITE_LIMIT_LENGTH).unwrap(),
            MAX_SQL_VALUE_BYTES as i32
        );
        let result: Result<Vec<u8>, _> = cache.connection.query_row(
            "SELECT zeroblob(?1)",
            [(MAX_SQL_VALUE_BYTES + 1) as i64],
            |r| r.get(0),
        );
        assert!(result.is_err());
    }

    #[test]
    fn excessive_sql_work_is_interrupted_and_next_operation_recovers() {
        let cache = Cache::memory().unwrap();
        reset_sql_budget(&cache.connection);
        let result: Result<i64, _> = cache.connection.query_row(
            "WITH RECURSIVE numbers(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM numbers WHERE n<1000000000) SELECT sum(n) FROM numbers",
            [], |r| r.get(0),
        );
        assert!(
            matches!(result, Err(rusqlite::Error::SqliteFailure(error, _)) if error.code == rusqlite::ErrorCode::OperationInterrupted)
        );
        assert_eq!(cache.stats().unwrap().automatic_records, 0);
    }
}
