use super::*;

#[test]
fn backup_v4_compact_paths_restore_original_names_and_distinct_bytes() {
    check_snapshot_paths(4);
}

#[test]
fn backup_v5_shared_object_paths_remain_unchanged() {
    check_snapshot_paths(5);
}

fn check_snapshot_paths(version: i64) {
    let root = std::env::temp_dir().join(format!("ca-backup-path-{}", crate::random_url_token()));
    fs::create_dir_all(root.join("source")).unwrap();
    let source = SqliteStore::open(root.join("source/db.sqlite")).unwrap();
    let tenant = "qa-short-path";
    set_folder(&source, tenant.into(), root.join("backups").to_string_lossy().into()).unwrap();
    source.upsert_work_note(json!({"tenantId":tenant,"pageId":"page","title":"Fixture","blocks":[],"markdown":""})).unwrap();
    let attachment_name = format!("{}.docx", "수학_관찰기록_".repeat(16));
    let media_name = format!("{}.png", "관찰사진_".repeat(16));
    for (index, base64, bytes) in [(0, "Zmlyc3Q=", b"first".as_slice()), (1, "c2Vjb25k", b"second".as_slice())] {
        source.upsert_board_media(json!({"tenantId":tenant,"boardId":"board","postId":"post",
            "mediaId":format!("media-{index}"),"fileName":media_name,"contentType":"image/png","dataBase64":base64})).unwrap();
        crate::work_note_attachments::save(&source, tenant.into(), format!("attachment-{index}"),
            "page".into(), format!("block-{index}"), attachment_name.clone(), "application/octet-stream".into(),
            &mut std::io::Cursor::new(bytes)).unwrap();
    }
    let snapshot = run_with_kind_version(&source, tenant.into(), "auto_sync", Some(1), version).unwrap();
    assert_eq!(snapshot["ok"], true);
    let manifest_path = Path::new(snapshot["manifestPath"].as_str().unwrap());
    let manifest = read_manifest(manifest_path).unwrap();
    let authoritative = authoritative_restore_manifest(manifest_path, &manifest, tenant).unwrap();
    let mut paths = HashSet::new();
    for (group, original_name, prefix) in [("media", &media_name, "board-media/"),
        ("workNoteAttachments", &attachment_name, "work-note-attachments/")] {
        let records = authoritative[group]["records"].as_array().unwrap();
        assert_eq!(records.len(), 2);
        for record in records {
            let relative = record["backupRelativePath"].as_str().unwrap();
            assert_eq!(record["fileName"].as_str(), Some(original_name.as_str()));
            if version == 4 {
                assert!(relative.starts_with(prefix));
                assert_eq!(relative.split('/').count(), 2);
                assert!(relative.len() <= 28, "long names and IDs must not lengthen the v4 locator: {relative}");
                assert!(!relative.contains(snapshot["backupId"].as_str().unwrap()));
                assert!(paths.insert(relative.to_string()), "distinct records must not overwrite each other");
            } else {
                assert!(relative.starts_with("objects/sha256/"));
                assert!(relative.ends_with(record["sha256"].as_str().unwrap()));
            }
            let path = crate::backup_v5::artifact_path(manifest_path, version, Path::new(relative)).unwrap();
            let (size, hash) = sha256_file(&path).unwrap();
            assert_eq!(Some(size), record["size"].as_u64());
            assert_eq!(Some(hash.as_str()), record["sha256"].as_str());
        }
    }
    fs::create_dir_all(root.join("restored")).unwrap();
    let restored = SqliteStore::open(root.join("restored/db.sqlite")).unwrap();
    set_folder(&restored, tenant.into(), root.join("backups").to_string_lossy().into()).unwrap();
    let result = crate::backup::restore_generation(&restored, tenant, manifest_path, 1, "verified", false).unwrap();
    assert_eq!(result["ok"], true);
    for row in list_media_rows(&source, tenant).unwrap() {
        assert_eq!(fs::read(source.data_dir.join(&row.local_path)).unwrap(), fs::read(restored.data_dir.join(&row.local_path)).unwrap());
    }
    for row in list_work_note_attachment_rows(&source, tenant).unwrap() {
        let readback = crate::work_note_attachments::open(&restored, tenant.into(), row.attachment_id).unwrap();
        assert_eq!(readback.record.file_name, attachment_name);
        assert_eq!(fs::read(source.data_dir.join(&row.local_path)).unwrap(), fs::read(restored.data_dir.join(&row.local_path)).unwrap());
    }
    assert_eq!(list_media_rows(&restored, tenant).unwrap().len(), 2);
    assert_eq!(list_work_note_attachment_rows(&restored, tenant).unwrap().len(), 2);
    drop(restored);
    drop(source);
    fs::remove_dir_all(root).unwrap();
}
