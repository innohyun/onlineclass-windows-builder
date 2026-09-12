use super::*;
use std::io::Cursor;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("classaimate-qa-restore-path-{}", crate::random_url_token()));
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn restore_target_paths_accept_both_separators_and_reject_escape() {
    for namespace in ["board-media", "work-note-attachments"] {
        let native = Path::new(namespace).join("qa").join("file.txt");
        assert_eq!(restore_target_path(&format!("{namespace}/qa/file.txt"), namespace), Some(native.clone()));
        assert_eq!(restore_target_path(&format!(r"{namespace}\qa\file.txt"), namespace), Some(native));
        for invalid in [
            String::new(), namespace.into(), "../outside.txt".into(), r"..\outside.txt".into(),
            format!("{namespace}/../outside.txt"), format!(r"{namespace}\..\outside.txt"),
            format!(r"{namespace}\../outside.txt"), format!("/{namespace}/file.txt"),
            format!(r"\{namespace}\file.txt"), format!(r"C:\{namespace}\file.txt"),
            format!("C:{namespace}/file.txt"), format!(r"\\server\share\{namespace}\file.txt"),
            format!(r"\\?\C:\{namespace}\file.txt"), format!("{namespace}/file.txt:stream"),
            format!("{namespace}-other/file.txt"), "unrelated/file.txt".into(),
        ] {
            assert!(restore_target_path(&invalid, namespace).is_none(), "accepted unsafe fixture path: {invalid}");
        }
    }
}

#[cfg(unix)]
#[test]
fn normalized_restore_target_still_rejects_symlink_escape() {
    let fixture = Fixture::new();
    let root = fixture.0.join("local");
    let outside = fixture.0.join("outside");
    fs::create_dir_all(root.join(".restore-staging/qa/staged")).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("sentinel.txt"), b"keep").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("work-note-attachments")).unwrap();
    let staged = PathBuf::from(".restore-staging/qa/staged/attachment");
    fs::write(root.join(&staged), b"incoming").unwrap();
    let conn = Connection::open_in_memory().unwrap();
    crate::restore_journal::install(&conn).unwrap();
    let intent = crate::restore_journal::Intent {
        operation_id: "qa".into(), staging_root: ".restore-staging/qa".into(),
        files: vec![crate::restore_journal::Media {
            staged, target: restore_target_path(r"work-note-attachments\sentinel.txt", "work-note-attachments").unwrap(),
            rollback: ".restore-staging/qa/rollback/attachment".into(),
            incoming_sha256: crate::restore_journal::digest(&root.join(".restore-staging/qa/staged/attachment")).unwrap(),
            previous_sha256: None,
        }],
    };
    assert_eq!(crate::restore_journal::prepare(&conn, &root, "qa", 1, "qa", &intent).unwrap_err(), "restore_recovery_required");
    assert_eq!(fs::read(outside.join("sentinel.txt")).unwrap(), b"keep");
    assert_eq!(conn.query_row("SELECT COUNT(*) FROM local_store_restore_journal", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
}

#[test]
fn windows_generation_restores_native_attachment_paths_and_bytes() {
    let fixture = Fixture::new();
    // Shared archive helpers also stay inside this fixture, even indirectly.
    crate::shared_archive::with_test_root(&fixture.0.join("archives"), || {
        let tenant = "qa-windows-path";
        let source = SqliteStore::open(fixture.0.join("source/store.sqlite")).unwrap();
        let target = SqliteStore::open(fixture.0.join("target/store.sqlite")).unwrap();
        let backup_root = fixture.0.join("transport");
        set_folder(&source, tenant.into(), backup_root.to_string_lossy().into()).unwrap();
        set_folder(&target, tenant.into(), backup_root.to_string_lossy().into()).unwrap();
        source.upsert_work_note(json!({"tenantId":tenant,"pageId":"page","title":"Synthetic fixture","blocks":[],"markdown":"Synthetic note"})).unwrap();
        let attachment_bytes = b"synthetic Windows attachment\r\n";
        crate::work_note_attachments::save(&source, tenant.into(), "attachment".into(), "page".into(),
            "block".into(), "fixture.txt".into(), "text/plain".into(), &mut Cursor::new(attachment_bytes)).unwrap();
        source.upsert_board_media(json!({"tenantId":tenant,"boardId":"board","postId":"post", "mediaId":"media",
            "fileName":"fixture.bin","contentType":"application/octet-stream","dataBase64":"bWVkaWE="})).unwrap();

        // Emulate Windows' stored local_path before the normal exporter seals it.
        // On Unix this is a fixture filename with backslashes, never a user file.
        let attachment = list_work_note_attachment_rows(&source, tenant).unwrap().remove(0);
        let media = list_media_rows(&source, tenant).unwrap().remove(0);
        for (table, id_column, id, local_path, bytes) in [
            ("work_note_attachments", "attachment_id", "attachment", attachment.local_path, attachment_bytes.as_slice()),
            ("board_media_files", "media_id", "media", media.local_path, b"media".as_slice()),
        ] {
            let windows_path = local_path.replace('/', "\\");
            fs::write(source.data_dir.join(&windows_path), bytes).unwrap();
            source.conn.lock().unwrap().execute(&format!("UPDATE {table} SET local_path=?1 WHERE tenant_id=?2 AND {id_column}=?3"),
                params![windows_path, tenant, id]).unwrap();
        }
        let snapshot = run_with_kind(&source, tenant.into(), "auto_sync", Some(1)).unwrap();
        let manifest_path = Path::new(snapshot["manifestPath"].as_str().unwrap());
        let manifest = read_manifest(manifest_path).unwrap();
        let authoritative = authoritative_restore_manifest(manifest_path, &manifest, tenant).unwrap();
        for group in ["media", "workNoteAttachments"] {
            assert!(authoritative[group]["records"][0]["localPath"].as_str().unwrap().contains('\\'));
        }

        let result = crate::backup::restore_generation(&target, tenant, manifest_path, 1, "announced", false).unwrap();
        assert_eq!(result["applied"], true);
        let restored_attachment = crate::work_note_attachments::open(&target, tenant.into(), "attachment".into()).unwrap();
        assert_eq!(restored_attachment.record.file_name, "fixture.txt");
        let mut bytes = Vec::new();
        restored_attachment.file.take(1024).read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, attachment_bytes);
        for (path, prefix, expected) in [
            (list_work_note_attachment_rows(&target, tenant).unwrap().remove(0).local_path, "work-note-attachments", attachment_bytes.as_slice()),
            (list_media_rows(&target, tenant).unwrap().remove(0).local_path, "board-media", b"media".as_slice()),
        ] {
            assert!(Path::new(&path).starts_with(prefix));
            assert!(Path::new(&path).components().count() > 1);
            assert_eq!(fs::read(target.data_dir.join(path)).unwrap(), expected);
        }
        assert_eq!(local_sync_state(&target, tenant).unwrap().applied_generation, 1);
        assert_eq!(target.conn.lock().unwrap().query_row("SELECT COUNT(*) FROM local_store_restore_journal", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
        target.restore_ready(tenant).unwrap();
    });
}
