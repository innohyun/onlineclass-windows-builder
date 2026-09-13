use super::*;

struct Fixture {
    root: PathBuf,
    store: Option<SqliteStore>,
}

impl Fixture {
    fn legacy_v1() -> Self {
        let root = std::env::temp_dir().join(format!(
            "classaimate-tracking-seed-{}",
            crate::random_url_token()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("fixture.sqlite");
        let store = SqliteStore::open(path.clone()).unwrap();
        {
            let conn = store.conn.lock().unwrap();
            // A pre-evidence installation has no triggers for the new ledger tables.
            for table in
                syncable_tables().filter(|table| table.name.starts_with("observation_evidence_"))
            {
                for action in ["insert", "update", "delete"] {
                    conn.execute_batch(&format!(
                        "DROP TRIGGER local_store_sync_{}_{action}",
                        table.name
                    ))
                    .unwrap();
                }
            }
            conn.execute("INSERT INTO lesson_observations VALUES('qa-seed','legacy','2026-09-13',1,'synthetic',?1,10)",
                params![json!({"tenantId":"qa-seed","docId":"legacy","date":"2026-09-13","period":1,
                    "studentCode":"synthetic","note":"synthetic legacy note","createdAtMs":10,"updatedAtMs":10}).to_string()]).unwrap();
            conn.execute_batch("UPDATE local_store_device_sync_state SET seed_version=1, applied_generation=338,
                published_generation=338, latest_generation=338, first_dirty_at_ms=NULL, last_dirty_at_ms=NULL, change_sequence=40;
                UPDATE local_store_device_sync_records SET changed_generation=338, record_version=7;
                INSERT INTO local_store_device_sync_records VALUES('qa-seed','observation_evidence_revisions',
                  '[\"must-stay-deleted\"]',330,9,337,1,123);").unwrap();
        }
        drop(store);
        // Exercise the actual startup migration: baseline creation precedes trigger installation.
        let store = SqliteStore::open(path).unwrap();
        let fixture = Self {
            root,
            store: Some(store),
        };
        assert_eq!(fixture.missing_ledger_tracking(), 2);
        fixture
    }

    fn store(&self) -> &SqliteStore {
        self.store.as_ref().unwrap()
    }

    fn missing_ledger_tracking(&self) -> i64 {
        let conn = self.store().conn.lock().unwrap();
        [
            "observation_evidence_revisions",
            "observation_evidence_batches",
        ]
        .into_iter()
        .map(|name| {
            let table = syncable_tables().find(|table| table.name == name).unwrap();
            let key = record_key_expression(name, table);
            conn.query_row(
                &format!(
                    "SELECT COUNT(*) FROM {name} WHERE tenant_id='qa-seed' AND NOT EXISTS (
                SELECT 1 FROM local_store_device_sync_records r WHERE r.tenant_id={name}.tenant_id
                AND r.table_name='{name}' AND r.record_key={key})"
                ),
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
        })
        .sum()
    }

    fn tracking(&self) -> Vec<String> {
        let conn = self.store().conn.lock().unwrap();
        let mut statement = conn.prepare("SELECT json_array(tenant_id,table_name,record_key,dirty_base_generation,
            record_version,changed_generation,tombstone,changed_at_ms) FROM local_store_device_sync_records
            ORDER BY tenant_id,table_name,record_key").unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    fn state(&self) -> String {
        self.store().conn.lock().unwrap().query_row("SELECT json_array(applied_generation,published_generation,
            latest_generation,first_dirty_at_ms,last_dirty_at_ms,change_sequence,seed_version,applying,tracking_repair_sequence)
            FROM local_store_device_sync_state WHERE tenant_id='qa-seed'", [], |row| row.get(0)).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        drop(self.store.take());
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn tracking_seed_upgrade_covers_actual_legacy_baseline_without_rewriting_existing_authority() {
    let fixture = Fixture::legacy_v1();
    let before = fixture.tracking();
    let state_before = local_sync_state(fixture.store(), "qa-seed").unwrap();
    seed_sync_records(fixture.store(), "qa-seed").unwrap();
    assert_eq!(
        fixture.missing_ledger_tracking(),
        0,
        "new ledger rows must enter the sealed apply index"
    );
    let after = fixture.tracking();
    assert_eq!(after.len(), before.len() + 2);
    assert!(
        before.iter().all(|row| after.contains(row)),
        "old versions and tombstones must stay byte-for-byte unchanged"
    );
    let conn = fixture.store().conn.lock().unwrap();
    let manifest = sync_manifest(&conn, "qa-seed", 339).unwrap();
    for row in manifest["records"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| {
            row["table"]
                .as_str()
                .unwrap()
                .starts_with("observation_evidence_")
                && row["tombstone"] == false
        })
    {
        assert_eq!(row["dirtyBaseGeneration"], 338);
        assert_eq!(row["recordVersion"], 1);
        assert_eq!(row["changedGeneration"], 339);
    }
    let seed: i64 = conn
        .query_row(
            "SELECT seed_version FROM local_store_device_sync_state WHERE tenant_id='qa-seed'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(seed, 2);
    drop(conn);
    let state_after = local_sync_state(fixture.store(), "qa-seed").unwrap();
    assert_eq!(
        state_after.change_sequence,
        state_before.change_sequence + 1
    );
    assert_eq!(
        state_after.tracking_repair_sequence,
        state_after.change_sequence
    );
    assert_eq!(
        state_after.applied_generation,
        state_before.applied_generation
    );
    assert_eq!(
        state_after.published_generation,
        state_before.published_generation
    );
    assert_eq!(
        state_after.latest_generation,
        state_before.latest_generation
    );
}

#[test]
fn tracking_seed_partial_failure_rolls_back_every_record_and_marker() {
    let fixture = Fixture::legacy_v1();
    fixture.store().conn.lock().unwrap().execute_batch("UPDATE local_store_device_sync_state SET seed_version=0;
        CREATE TRIGGER synthetic_seed_failure BEFORE INSERT ON local_store_device_sync_records
        WHEN NEW.table_name='observation_evidence_batches' BEGIN SELECT RAISE(ABORT,'synthetic disk failure'); END;").unwrap();
    let before = fixture.tracking();
    let state_before = fixture.state();
    let error = seed_sync_records(fixture.store(), "qa-seed").unwrap_err();
    assert!(error.starts_with("db_sync_tracking_seed_failed:"));
    assert_eq!(
        fixture.tracking(),
        before,
        "earlier tables must not leak out of a failed upgrade"
    );
    assert_eq!(fixture.state(), state_before);
}

#[test]
fn tracking_seed_upgrade_refuses_in_progress_apply_without_mutation() {
    let fixture = Fixture::legacy_v1();
    fixture
        .store()
        .conn
        .lock()
        .unwrap()
        .execute("UPDATE local_store_device_sync_state SET applying=1", [])
        .unwrap();
    let before = fixture.tracking();
    let state_before = fixture.state();
    assert_eq!(
        seed_sync_records(fixture.store(), "qa-seed").unwrap_err(),
        "db_sync_seed_applying"
    );
    assert_eq!(fixture.tracking(), before);
    assert_eq!(fixture.state(), state_before);
}

#[test]
fn tracking_seed_does_not_downgrade_future_versions_or_mutate_repeated_reads() {
    let fixture = Fixture::legacy_v1();
    fixture
        .store()
        .conn
        .lock()
        .unwrap()
        .execute(
            "UPDATE local_store_device_sync_state SET seed_version=77",
            [],
        )
        .unwrap();
    let before = fixture.tracking();
    let state_before = fixture.state();
    let total_before = fixture.store().conn.lock().unwrap().total_changes();
    for _ in 0..40 {
        local_sync_state(fixture.store(), "qa-seed").unwrap();
        seed_sync_records(fixture.store(), "qa-seed").unwrap();
    }
    assert_eq!(fixture.tracking(), before);
    assert_eq!(fixture.state(), state_before);
    assert_eq!(
        fixture.store().conn.lock().unwrap().total_changes(),
        total_before
    );
}

#[test]
fn tracking_seed_catalog_changes_require_a_new_seed_version() {
    use sha2::{Digest, Sha256};
    let mut catalog = syncable_tables()
        .map(|table| {
            format!(
                "{}:{}:{}\n",
                table.name,
                table.columns.join(","),
                table.key_columns.join(",")
            )
        })
        .collect::<Vec<_>>();
    catalog.sort();
    // Append a new reviewed version/digest pair when the catalog changes; do not
    // rewrite an existing version's mapping and strand already-seeded tenants.
    let expected = match SYNC_RECORD_SEED_VERSION {
        2 => "b239f33a1ee7432a4428076d4637a06d249d0eb8d71d23e72e2c468a8c619337",
        _ => panic!("register the new seed version and its syncable table coverage"),
    };
    assert_eq!(
        format!("{:x}", Sha256::digest(catalog.concat().as_bytes())),
        expected,
        "syncable table/key/column catalog changed: bump the seed version and add coverage"
    );
}

#[test]
fn tracking_seed_marker_failure_rolls_back_backfill_and_dirty_state() {
    let fixture = Fixture::legacy_v1();
    fixture
        .store()
        .conn
        .lock()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER synthetic_marker_failure
        BEFORE UPDATE OF seed_version ON local_store_device_sync_state
        WHEN NEW.seed_version=2 BEGIN SELECT RAISE(ABORT,'synthetic marker failure'); END;",
        )
        .unwrap();
    let before = fixture.tracking();
    let state_before = fixture.state();
    assert!(seed_sync_records(fixture.store(), "qa-seed")
        .unwrap_err()
        .starts_with("db_sync_seed_version_failed:"));
    assert_eq!(fixture.tracking(), before);
    assert_eq!(fixture.state(), state_before);
}

#[test]
fn tracking_seed_external_writer_cannot_interleave_with_upgrade() {
    let fixture = Fixture::legacy_v1();
    fixture
        .store()
        .conn
        .lock()
        .unwrap()
        .busy_timeout(Duration::ZERO)
        .unwrap();
    let writer = Connection::open(&fixture.store().db_path).unwrap();
    let before = fixture.tracking();
    let state_before = fixture.state();
    writer.execute_batch("BEGIN IMMEDIATE; UPDATE local_store_device_sync_state SET applying=1 WHERE tenant_id='qa-seed';").unwrap();
    assert!(seed_sync_records(fixture.store(), "qa-seed")
        .unwrap_err()
        .starts_with("db_sync_seed_begin_failed:"));
    assert_eq!(fixture.tracking(), before);
    assert_eq!(fixture.state(), state_before);
    writer.execute_batch("COMMIT").unwrap();
    assert_eq!(
        seed_sync_records(fixture.store(), "qa-seed").unwrap_err(),
        "db_sync_seed_applying"
    );
    writer
        .execute(
            "UPDATE local_store_device_sync_state SET applying=0 WHERE tenant_id='qa-seed'",
            [],
        )
        .unwrap();
    seed_sync_records(fixture.store(), "qa-seed").unwrap();
    assert_eq!(fixture.missing_ledger_tracking(), 0);
}

#[test]
fn tracking_seed_completed_version_is_read_only_and_no_missing_rows_do_not_dirty() {
    let fixture = Fixture::legacy_v1();
    seed_sync_records(fixture.store(), "qa-seed").unwrap();
    let before = fixture.state();
    let tracking_before = fixture.tracking();
    let changes_before = fixture.store().conn.lock().unwrap().total_changes();
    for _ in 0..40 {
        seed_sync_records(fixture.store(), "qa-seed").unwrap();
        local_sync_state(fixture.store(), "qa-seed").unwrap();
    }
    assert_eq!(fixture.state(), before);
    assert_eq!(fixture.tracking(), tracking_before);
    assert_eq!(
        fixture.store().conn.lock().unwrap().total_changes(),
        changes_before
    );
    fixture
        .store()
        .conn
        .lock()
        .unwrap()
        .execute(
            "UPDATE local_store_device_sync_state SET seed_version=1,tracking_repair_sequence=0",
            [],
        )
        .unwrap();
    let state_before = local_sync_state(fixture.store(), "qa-seed").unwrap();
    seed_sync_records(fixture.store(), "qa-seed").unwrap();
    let state_after = local_sync_state(fixture.store(), "qa-seed").unwrap();
    assert_eq!(state_before.change_sequence, state_after.change_sequence);
    assert_eq!(
        state_before.first_dirty_at_ms,
        state_after.first_dirty_at_ms
    );
    assert_eq!(state_before.last_dirty_at_ms, state_after.last_dirty_at_ms);
    assert_eq!(state_after.tracking_repair_sequence, 0);
    assert_eq!(fixture.tracking(), tracking_before);
}

#[test]
fn tracking_seed_repair_survives_noop_and_old_publication_until_a_post_repair_capture() {
    let fixture = Fixture::legacy_v1();
    let before = local_sync_state(fixture.store(), "qa-seed").unwrap();
    let old_sync = sync_manifest(&fixture.store().conn.lock().unwrap(), "qa-seed", 339).unwrap();
    let old_path = fixture.root.join("old-v3-manifest.json");
    fs::write(&old_path, json!({"version":3,"sync":old_sync}).to_string()).unwrap();
    seed_sync_records(fixture.store(), "qa-seed").unwrap();
    let repair = local_sync_state(fixture.store(), "qa-seed").unwrap();
    let repaired_tracking = fixture.tracking();
    mark_sync_unchanged(fixture.store(), "qa-seed", 338, repair.change_sequence).unwrap();
    assert_eq!(fixture.tracking(), repaired_tracking);
    assert_eq!(
        local_sync_state(fixture.store(), "qa-seed")
            .unwrap()
            .tracking_repair_sequence,
        repair.change_sequence
    );
    mark_sync_published(
        fixture.store(),
        "qa-seed",
        339,
        &old_path,
        "old-content",
        "announced",
        before.change_sequence,
    )
    .unwrap();
    assert_eq!(
        local_sync_state(fixture.store(), "qa-seed")
            .unwrap()
            .tracking_repair_sequence,
        repair.change_sequence
    );
    let captured_path = fixture.root.join("captured.sqlite");
    let captured =
        super::super::capture::export(fixture.store(), "qa-seed", &captured_path, 340).unwrap();
    let captured_conn =
        Connection::open_with_flags(&captured_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    assert!(
        !table_exists(&captured_conn, "local_store_device_sync_state").unwrap(),
        "repair state must remain device-only"
    );
    drop(captured_conn);
    let fresh_path = fixture.root.join("fresh-v3-manifest.json");
    fs::write(
        &fresh_path,
        json!({"version":3,"sync":captured.sync}).to_string(),
    )
    .unwrap();
    let revision: String = fixture.store().conn.lock().unwrap().query_row(
        "SELECT json_extract(payload_json,'$.revisionId') FROM lesson_observations WHERE tenant_id='qa-seed'", [], |row| row.get(0)).unwrap();
    fixture.store().upsert_observation(json!({"tenantId":"qa-seed","docId":"legacy","dateKey":"2026-09-13",
        "period":1,"studentCode":"synthetic","observation":"later synthetic edit","expectedRevisionId":revision,
        "correctionReason":"synthetic concurrent edit"})).unwrap();
    mark_sync_published(
        fixture.store(),
        "qa-seed",
        340,
        &fresh_path,
        "new-content",
        "announced",
        captured.sequence,
    )
    .unwrap();
    let final_state = local_sync_state(fixture.store(), "qa-seed").unwrap();
    assert_eq!(final_state.tracking_repair_sequence, 0);
    assert!(
        final_state.first_dirty_at_ms > 0,
        "an edit after capture remains dirty after repair publication"
    );
    assert!(final_state.change_sequence > captured.sequence);
}
