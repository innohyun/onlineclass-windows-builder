//! Read-only, local revision-branch counts. This is not receipt/photo verification
//! or proof that another device's current observation projection is identical.
use crate::SqliteStore;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

struct Revision {
    doc: String,
    hash: String,
    parents: Vec<(String, String)>,
    record: Value,
}

fn text(value: &Value) -> Result<String, ()> {
    value
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or(())
}

fn count_branches(conn: &Connection, tenant: &str) -> Result<usize, ()> {
    if tenant.is_empty() {
        return Err(());
    }
    let mut revisions = HashMap::new();
    let mut statement = conn.prepare("SELECT revision_id,doc_id,payload_json,revision_hash FROM observation_evidence_revisions WHERE tenant_id=?1").map_err(|_| ())?;
    let rows = statement
        .query_map(params![tenant], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(|_| ())?;
    for row in rows {
        let (id, doc, raw, hash) = row.map_err(|_| ())?;
        let envelope: Value = serde_json::from_str(&raw).map_err(|_| ())?;
        if id.is_empty()
            || doc.is_empty()
            || hash.is_empty()
            || envelope["version"] != 1
            || envelope["tenantId"] != tenant
            || envelope["docId"] != doc
            || envelope["revisionId"] != id
            || !envelope["record"].is_object()
        {
            return Err(());
        }
        let parents = if envelope["eventKind"] == "resolve" {
            let ids = envelope["parentRevisionIds"].as_array().ok_or(())?;
            let hashes = envelope["parentRevisionHashes"].as_array().ok_or(())?;
            if ids.len() < 2 || ids.len() != hashes.len() {
                return Err(());
            }
            ids.iter()
                .zip(hashes)
                .map(|(id, hash)| Ok((text(id)?, text(hash)?)))
                .collect::<Result<Vec<_>, ()>>()?
        } else if envelope["previousRevisionId"].is_null() {
            if !envelope["previousRevisionHash"].is_null() {
                return Err(());
            }
            vec![]
        } else {
            vec![(
                text(&envelope["previousRevisionId"])?,
                text(&envelope["previousRevisionHash"])?,
            )]
        };
        let unique = parents.iter().map(|(id, _)| id).collect::<HashSet<_>>();
        if unique.len() != parents.len() {
            return Err(());
        }
        revisions.insert(
            id,
            Revision {
                doc,
                hash,
                parents,
                record: envelope["record"].clone(),
            },
        );
    }
    drop(statement);

    let mut children: HashMap<String, Vec<String>> = HashMap::new();
    let mut remaining = HashMap::new();
    let mut queue = Vec::new();
    for (id, revision) in &revisions {
        remaining.insert(id.clone(), revision.parents.len());
        if revision.parents.is_empty() {
            queue.push(id.clone());
        }
        for (parent_id, parent_hash) in &revision.parents {
            let parent = revisions.get(parent_id).ok_or(())?;
            if parent.doc != revision.doc || parent.hash != *parent_hash {
                return Err(());
            }
            children
                .entry(parent_id.clone())
                .or_default()
                .push(id.clone());
        }
    }
    // Kahn's traversal rejects cycles without recursion depth or repeated DAG walks.
    let mut visited = 0;
    while let Some(id) = queue.pop() {
        visited += 1;
        for child in children.get(&id).into_iter().flatten() {
            let pending = remaining.get_mut(child).ok_or(())?;
            *pending -= 1;
            if *pending == 0 {
                queue.push(child.clone());
            }
        }
    }
    if visited != revisions.len() {
        return Err(());
    }
    let mut heads: HashMap<&str, usize> = HashMap::new();
    for (id, revision) in &revisions {
        if !children.contains_key(id) {
            *heads.entry(&revision.doc).or_default() += 1;
        }
    }

    let mut current_docs = HashSet::new();
    let mut statement = conn
        .prepare("SELECT doc_id,payload_json FROM lesson_observations WHERE tenant_id=?1")
        .map_err(|_| ())?;
    let rows = statement
        .query_map(params![tenant], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|_| ())?;
    for row in rows {
        let (doc, raw) = row.map_err(|_| ())?;
        let mut current: Value = serde_json::from_str(&raw).map_err(|_| ())?;
        let id = text(&current["revisionId"])?;
        let revision = revisions.get(&id).ok_or(())?;
        if current["evidenceVersion"] != 1
            || current["tenantId"] != tenant
            || current["docId"] != doc
            || revision.doc != doc
            || current["revisionHash"] != revision.hash
            || children.contains_key(&id)
        {
            return Err(());
        }
        let record = current.as_object_mut().ok_or(())?;
        for key in ["evidenceVersion", "revisionId", "revisionHash"] {
            record.remove(key);
        }
        if current != revision.record || !current_docs.insert(doc) {
            return Err(());
        }
    }
    // Missing projections or projection-only imports must not look like zero branches.
    if current_docs.len() != heads.len() || heads.keys().any(|doc| !current_docs.contains(*doc)) {
        return Err(());
    }
    Ok(heads.values().filter(|count| **count > 1).count())
}

fn response(result: Result<usize, ()>) -> Value {
    match result {
        Ok(count) => {
            json!({"state":if count == 0 { "clear" } else { "conflict" },"conflictedRecordCount":count})
        }
        Err(()) => json!({"state":"unknown","conflictedRecordCount":null}),
    }
}

pub(crate) fn status(store: &SqliteStore, tenant: &str) -> Value {
    response((|| {
        let mut conn = store.conn.lock().map_err(|_| ())?;
        // Both SELECTs see one database snapshot, including writes by another process.
        let tx = conn.transaction().map_err(|_| ())?;
        let count = count_branches(&tx, tenant)?;
        tx.commit().map_err(|_| ())?;
        Ok(count)
    })())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&crate::observation_evidence::schema(""))
            .unwrap();
        conn.execute_batch("CREATE TABLE lesson_observations(tenant_id TEXT,doc_id TEXT,payload_json TEXT,PRIMARY KEY(tenant_id,doc_id))").unwrap();
        conn
    }

    fn revision(
        conn: &Connection,
        tenant: &str,
        doc: &str,
        id: &str,
        parents: &[(&str, &str)],
    ) -> String {
        let mut envelope = json!({"version":1,"tenantId":tenant,"docId":doc,"revisionId":id,
            "eventKind":if parents.len() > 1 { "resolve" } else { "create" },
            "previousRevisionId":parents.first().map(|(id,_)| *id),
            "previousRevisionHash":parents.first().map(|(_,hash)| *hash),
            "record":{"tenantId":tenant,"docId":doc,"note":"synthetic only"}});
        if parents.len() > 1 {
            envelope["parentRevisionIds"] =
                json!(parents.iter().map(|(id, _)| *id).collect::<Vec<_>>());
            envelope["parentRevisionHashes"] =
                json!(parents.iter().map(|(_, hash)| *hash).collect::<Vec<_>>());
        }
        let hash = crate::observation_evidence::hash(&envelope);
        conn.execute(
            "INSERT INTO observation_evidence_revisions VALUES(?1,?2,?3,?4,?5,1)",
            params![tenant, id, doc, envelope.to_string(), hash],
        )
        .unwrap();
        let mut current = envelope["record"].clone();
        current["evidenceVersion"] = json!(1);
        current["revisionId"] = json!(id);
        current["revisionHash"] = json!(hash);
        conn.execute("INSERT INTO lesson_observations VALUES(?1,?2,?3) ON CONFLICT(tenant_id,doc_id) DO UPDATE SET payload_json=excluded.payload_json", params![tenant,doc,current.to_string()]).unwrap();
        hash
    }

    #[test]
    fn branches_are_local_read_only_counts_and_resolve_clears_them() {
        let conn = fixture();
        let one = revision(&conn, "a", "doc", "one", &[]);
        let two = revision(&conn, "a", "doc", "two", &[]);
        revision(&conn, "a", "other", "other", &[]);
        let changes = conn.total_changes();
        assert_eq!(
            response(count_branches(&conn, "a")),
            json!({"state":"conflict","conflictedRecordCount":1})
        );
        assert_eq!(conn.total_changes(), changes);
        revision(
            &conn,
            "a",
            "doc",
            "resolution",
            &[("one", &one), ("two", &two)],
        );
        assert_eq!(
            response(count_branches(&conn, "a")),
            json!({"state":"clear","conflictedRecordCount":0})
        );
    }

    #[test]
    fn a_descendant_and_unrelated_tenant_are_not_branches() {
        let conn = fixture();
        let root = revision(&conn, "a", "doc", "one", &[]);
        revision(&conn, "a", "doc", "two", &[("one", &root)]);
        conn.execute("INSERT INTO observation_evidence_revisions VALUES('b','bad','bad','invalid json','bad',1)", []).unwrap();
        assert_eq!(count_branches(&conn, "a"), Ok(0));
        assert_eq!(count_branches(&conn, "empty"), Ok(0));
    }

    #[test]
    fn missing_revision_projection_parent_or_malformed_json_is_unknown() {
        for mutation in [
            "DELETE FROM observation_evidence_revisions",
            "DELETE FROM lesson_observations",
            "UPDATE observation_evidence_revisions SET payload_json='invalid json'",
            "UPDATE lesson_observations SET payload_json='invalid json'",
            "UPDATE observation_evidence_revisions SET payload_json=json_set(payload_json,'$.previousRevisionId','absent','$.previousRevisionHash','hash')",
            "UPDATE lesson_observations SET payload_json=json_set(payload_json,'$.revisionHash','different')",
        ] {
            let conn = fixture();
            revision(&conn, "a", "doc", "one", &[]);
            conn.execute_batch(mutation).unwrap();
            assert_eq!(response(count_branches(&conn, "a")), json!({"state":"unknown","conflictedRecordCount":null}));
        }
    }

    #[test]
    fn cycles_and_cross_document_parent_links_are_unknown() {
        let conn = fixture();
        let one = revision(&conn, "a", "doc", "one", &[]);
        let two = revision(&conn, "a", "doc", "two", &[("one", &one)]);
        conn.execute("UPDATE observation_evidence_revisions SET payload_json=json_set(payload_json,'$.previousRevisionId','two','$.previousRevisionHash',?1) WHERE revision_id='one'", params![two]).unwrap();
        assert!(count_branches(&conn, "a").is_err());
        let conn = fixture();
        let other = revision(&conn, "a", "other-doc", "other", &[]);
        revision(&conn, "a", "doc", "one", &[("other", &other)]);
        assert!(count_branches(&conn, "a").is_err());
    }

    #[test]
    fn database_failure_and_legacy_projection_are_unknown() {
        let conn = fixture();
        conn.execute("INSERT INTO lesson_observations VALUES('a','old','{}')", [])
            .unwrap();
        assert!(count_branches(&conn, "a").is_err());
        conn.execute_batch("DROP TABLE observation_evidence_revisions")
            .unwrap();
        assert_eq!(
            response(count_branches(&conn, "a")),
            json!({"state":"unknown","conflictedRecordCount":null})
        );
    }
}
