use super::{material_assets, receipt_verification, transaction_store::TransactionStore};
use crate::{canonicalize_json, SqliteStore};
use chrono::Utc;
use regex::Regex;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::HashMap;

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}
fn rows(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn invalid() -> String {
    "classaimate_mcp_write_job_invalid".into()
}
fn db(error: rusqlite::Error) -> String {
    format!("db_lesson_snapshot_failed:{error}")
}
fn attachment(value: &Value) -> Option<Value> {
    let node = if value["content"].is_object() {
        &value["content"]
    } else {
        value
    };
    let attrs = &node["attrs"];
    let asset = [
        text(value, "assetId"),
        text(value, "localAttachmentId"),
        text(value, "attachmentId"),
        text(attrs, "attachmentId"),
    ]
    .into_iter()
    .find(|id| !id.is_empty())?;
    if !matches!(text(value, "type"), "attachment" | "image")
        && !matches!(
            text(node, "type"),
            "attachmentBlock" | "studentMaterialImage"
        )
    {
        return None;
    }
    let id = [text(value, "id"), text(attrs, "blockId")]
        .into_iter()
        .find(|v| !v.is_empty())
        .unwrap_or("");
    let kind = [
        text(value, "attachmentType"),
        text(value, "kind"),
        text(attrs, "kind"),
    ]
    .into_iter()
    .find(|v| !v.is_empty())
    .unwrap_or("file");
    let name = [
        text(value, "alt"),
        text(value, "text"),
        text(value, "fileName"),
        text(attrs, "alt"),
        text(attrs, "fileName"),
    ]
    .into_iter()
    .find(|v| !v.trim().is_empty())
    .unwrap_or("수업 참고 이미지");
    Some(json!({"id":id,"assetId":asset,"kind":kind,"name":name,"nodeType":text(node,"type")}))
}
fn project(value: &Value) -> Value {
    if let Some(array) = value.as_array() {
        return Value::Array(array.iter().map(project).collect());
    }
    if !value.is_object() {
        return value.clone();
    }
    if let Some(info) = attachment(value) {
        let id = text(&info, "id");
        let asset = text(&info, "assetId");
        let image = asset.starts_with("lesson-material-asset-")
            && asset.len() == 62
            && asset[22..].chars().all(|c| c.is_ascii_hexdigit());
        let mut projected = if text(value, "type") == "studentMaterialImage" && image {
            json!({"type":"studentMaterialImage","attrs":{"attachmentId":asset,"alt":text(&info,"name").trim()}})
        } else if image {
            json!({"type":"image","assetId":asset,"alt":text(&info,"name").trim()})
        } else if text(value, "type") == "attachmentBlock" {
            json!({"type":"attachmentBlock","attrs":{"attachmentId":asset,"kind":text(&info,"kind")}})
        } else {
            json!({"type":"attachment","localAttachmentId":asset,"attachmentType":text(&info,"kind")})
        };
        if !id.is_empty() {
            if projected.get("attrs").is_some() {
                projected["attrs"]["blockId"] = json!(id);
            } else {
                projected["id"] = json!(id);
            }
        }
        return projected;
    }
    Value::Object(
        value
            .as_object()
            .unwrap()
            .iter()
            .map(|(key, value)| (key.clone(), project(value)))
            .collect(),
    )
}
fn markdown_projection(value: &str) -> String {
    let links = Regex::new(r"!?\[[^\]]*\]\(local-attachment://[^)]+\)").unwrap();
    let raw = Regex::new(r"local-attachment://[^\s)]+").unwrap();
    raw.replace_all(&links.replace_all(value.trim(), "[첨부]"), "[첨부]")
        .into_owned()
}
fn snapshot(page: &Value) -> Value {
    json!({"documentTitle":text(page,"title").trim(),"documentBlocks":project(&page["blocks"]),"markdown":markdown_projection(text(page,"markdown"))})
}
fn digest(value: &Value) -> Result<String, String> {
    crate::sha256_json(&canonicalize_json(value))
}

pub(crate) fn inspect(store: &SqliteStore, input: &Value) -> Result<Value, String> {
    let tenant = text(input, "tenantId");
    let page_id = text(input, "pageRef");
    let plan_id = text(input, "planId");
    if tenant.is_empty() || page_id.is_empty() || plan_id.is_empty() { return Err(invalid()); }
    let bindings = crate::lesson_plan_bindings::list(store, tenant.into())?;
    let selected = bindings.iter().find(|row| row["planId"] == plan_id || row["pageId"] == page_id);
    if selected.is_some_and(|row| row["planId"] != plan_id || row["pageId"] != page_id) {
        return Err("INVALID_RELATION".into());
    }
    let conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
    let page = TransactionStore { conn: &conn }.get_work_note(tenant.into(), page_id.into())?;
    match page {
        None => Ok(json!({"status":"missing","binding":selected})),
        Some(page) => {
            if page["properties"]["_localTrash"]["deletedAtMs"].as_i64().unwrap_or(0) > 0 {
                return Err("LESSON_MATERIAL_ARCHIVED".into());
            }
            Ok(json!({"status":"found","binding":selected,"revision":page["updatedAtMs"],"snapshot":snapshot(&page)}))
        }
    }
}

fn binding(conn: &Connection, tenant: &str, data: &Value) -> Result<(), String> {
    let a = &data["materialAuthority"];
    if data["v"] != 1
        || a["v"] != 1
        || text(a, "tenantId") != tenant
        || a["storageKind"] != "local"
        || data["pageRef"] != a["localPageId"]
        || data["snapshotRevision"].as_i64() != a["revision"].as_i64().map(|v| v + 1)
        || data["attachments"].as_array().is_none_or(|v| v.len() > 12)
        || digest(&data["baseSnapshot"])? != text(a, "contentSha256")
        || digest(&data["targetSnapshot"])? != text(data, "contentSha256")
    {
        return Err(invalid());
    }
    let row:Option<Value>=conn.query_row("SELECT page_id,plan_kind,date_key,start_period,end_period,binding_revision FROM lesson_plan_bindings WHERE tenant_id=?1 AND plan_id=?2",
        params![tenant,text(a,"planId")],|r|Ok(json!({"localPageId":r.get::<_,String>(0)?,"planKind":r.get::<_,String>(1)?,"dateKey":r.get::<_,String>(2)?,"startPeriod":r.get::<_,i64>(3)?,"endPeriod":r.get::<_,i64>(4)?,"bindingRevision":r.get::<_,i64>(5)?}))).optional().map_err(db)?;
    if data["transform"]["strategy"] == "create_local" {
        if row.is_some() { return Err("INVALID_RELATION".into()); }
        let page_exists: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM work_note_pages WHERE tenant_id=?1 AND page_id=?2)",params![tenant,text(data,"pageRef")],|r|r.get(0)).map_err(db)?;
        return if page_exists { Err("MATERIAL_REVISION_CONFLICT".into()) } else { Ok(()) };
    }
    if row.is_none_or(|r| {
        let old = &data["transform"]["localBinding"];
        if old["pageId"] == a["localPageId"] && old["planId"] == a["planId"]
            && old["bindingRevision"].as_i64().unwrap_or(0) < a["bindingRevision"].as_i64().unwrap_or(0)
            && r["localPageId"] == old["pageId"]
            && ["planKind","dateKey","startPeriod","endPeriod","bindingRevision"].iter().all(|key| r[key] == old[key]) { return false; }
        [
            "localPageId",
            "planKind",
            "dateKey",
            "startPeriod",
            "endPeriod",
            "bindingRevision",
        ]
        .iter()
        .any(|key| r[key] != a[key])
    }) {
        return Err("INVALID_RELATION".into());
    }
    Ok(())
}

fn raw_attachment_node(value: &Value) -> Option<Value> {
    let info = attachment(value)?;
    let node = if value["content"].is_object() {
        &value["content"]
    } else {
        value
    };
    if text(node, "type") == "attachmentBlock" {
        return Some(node.clone());
    }
    if text(node, "type") == "studentMaterialImage" {
        return Some(node.clone());
    }
    Some(
        json!({"type":"attachmentBlock","attrs":{"blockId":info["id"],"indent":value["indent"].as_i64().unwrap_or(0).clamp(0,3),
        "attachmentId":info["assetId"],"fileName":text(value,"fileName"),"contentType":text(value,"contentType"),"size":value["size"].as_i64().unwrap_or(0),
        "kind":info["kind"],"displayMode":value["displayMode"].as_str().unwrap_or("file")}}),
    )
}
fn attachment_nodes(value: &Value, output: &mut HashMap<String, Value>) {
    if let Some(info) = attachment(value) {
        if let Some(node) = raw_attachment_node(value) {
            output.entry(text(&info, "assetId").into()).or_insert(node);
        }
    }
    if let Some(array) = value.as_array() {
        for child in array {
            attachment_nodes(child, output);
        }
    }
    if let Some(object) = value.as_object() {
        for child in object.values() {
            attachment_nodes(child, output);
        }
    }
}
fn rehydrate(value: &Value, originals: &HashMap<String, Value>) -> Result<Value, String> {
    if let Some(array) = value.as_array() {
        return array
            .iter()
            .map(|v| rehydrate(v, originals))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array);
    }
    if let Some(info) = attachment(value) {
        let original = originals
            .get(text(&info, "assetId"))
            .ok_or("MATERIAL_REFERENCE_CONFLICT")?;
        let mut node = original.clone();
        node["attrs"]["blockId"] = info["id"].clone();
        if text(value, "type") == "attachmentBlock" || text(value, "type") == "studentMaterialImage"
        {
            return Ok(node);
        }
        // Root attachment wrappers carry the original native metadata and the
        // same rich node that the common document transformer would produce.
        return Ok(
            json!({"id":info["id"],"type":"attachment","text":node["attrs"]["fileName"],"content":node,
            "attachmentId":info["assetId"],"fileName":original["attrs"]["fileName"],"contentType":original["attrs"]["contentType"],
            "size":original["attrs"]["size"],"kind":info["kind"],"displayMode":original["attrs"]["displayMode"]}),
        );
    }
    if let Some(object) = value.as_object() {
        return object
            .iter()
            .map(|(key, value)| Ok((key.clone(), rehydrate(value, originals)?)))
            .collect::<Result<serde_json::Map<_, _>, String>>()
            .map(Value::Object);
    }
    Ok(value.clone())
}
fn inline(node: &Value) -> String {
    if node["type"] == "text" {
        let mut value = text(node, "text").to_string();
        for mark in rows(&node["marks"]) {
            value = match text(mark, "type") {
                "bold" => format!("**{value}**"),
                "italic" => format!("*{value}*"),
                "strike" => format!("~~{value}~~"),
                "code" => format!("`{value}`"),
                "link" => format!("[{value}]({})", text(&mark["attrs"], "href")),
                _ => value,
            };
        }
        return value;
    }
    if node["type"] == "hardBreak" {
        return "  \n".into();
    }
    if node["type"] == "userMention" {
        return format!("@{}", text(&node["attrs"], "label").trim());
    }
    rows(&node["content"]).iter().map(inline).collect()
}
fn node_markdown(node: &Value, depth: usize) -> String {
    let attrs = &node["attrs"];
    let children = rows(&node["content"]);
    let rendered = |separator: &str| {
        children
            .iter()
            .map(|v| node_markdown(v, depth))
            .collect::<Vec<_>>()
            .join(separator)
    };
    match text(node, "type") {
        "text" => inline(node),
        "h1" | "h2" | "h3" => format!("{} {}", "#".repeat(text(node, "type")[1..].parse::<usize>().unwrap_or(1)), text(node, "text")),
        "bullet" => format!("- {}", text(node, "text")),
        "number" => format!("1. {}", text(node, "text")),
        "todo" => format!("- [{}] {}", if node["checked"] == true || node["done"] == true { "x" } else { " " }, text(node, "text")),
        "quote" => format!("> {}", text(node, "text")),
        "code" => format!("```\n{}\n```", text(node, "text")),
        "divider" => "---".into(),
        "paragraph" => inline(node),
        "heading" => format!(
            "{} {}",
            "#".repeat(attrs["level"].as_u64().unwrap_or(1).clamp(1, 6) as usize),
            inline(node)
        ),
        "blockquote" => rendered("\n")
            .split('\n')
            .map(|v| format!("> {v}"))
            .collect::<Vec<_>>()
            .join("\n"),
        "codeBlock" => format!("```{}\n{}\n```", text(attrs, "language"), inline(node)),
        "horizontalRule" => "---".into(),
        "bulletList" | "orderedList" | "taskList" => children
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let prefix = match text(node, "type") {
                    "orderedList" => {
                        format!("{}.", i as u64 + attrs["start"].as_u64().unwrap_or(1))
                    }
                    "taskList" => format!(
                        "- [{}]",
                        if item["attrs"]["checked"] == true {
                            "x"
                        } else {
                            " "
                        }
                    ),
                    _ => "-".into(),
                };
                format!(
                    "{}{prefix} {}",
                    "  ".repeat(depth),
                    rows(&item["content"])
                        .iter()
                        .map(|v| node_markdown(v, depth + 1))
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        "details" => {
            let summary = children
                .iter()
                .find(|v| v["type"] == "detailsSummary")
                .unwrap_or(&Value::Null);
            let body = children
                .iter()
                .find(|v| v["type"] == "detailsContent")
                .unwrap_or(&Value::Null);
            format!(
                "<details>\n<summary>{}</summary>\n\n{}\n</details>",
                inline(summary),
                rows(&body["content"])
                    .iter()
                    .map(|v| node_markdown(v, depth))
                    .collect::<Vec<_>>()
                    .join("\n\n")
            )
        }
        "callout" => format!(
            "> [!NOTE] {}\n{}",
            attrs["icon"].as_str().unwrap_or("💡"),
            children
                .iter()
                .map(|v| format!("> {}", node_markdown(v, depth)))
                .collect::<Vec<_>>()
                .join("\n")
        ),
        "pageLinkBlock" => format!(
            "[[worknote:{}|{}]]",
            text(attrs, "pageId"),
            attrs["title"].as_str().unwrap_or("제목 없음")
        ),
        "attachmentBlock" => format!(
            "{}[{}](local-attachment://{})",
            if attrs["kind"] == "image" { "!" } else { "" },
            attrs["fileName"]
                .as_str()
                .unwrap_or("첨부파일")
                .replace(']', "\\]"),
            text(attrs, "attachmentId")
        ),
        "studentMaterialImage" => "[첨부]".into(),
        "table" => {
            let cells: Vec<_> = children
                .iter()
                .map(|r| {
                    rows(&r["content"])
                        .iter()
                        .map(|c| rows(&c["content"]).iter().map(inline).collect::<Vec<_>>().join("<br>").replace('|', "\\|"))
                        .collect::<Vec<_>>()
                        .join(" | ")
                })
                .collect();
            if cells.is_empty() {
                String::new()
            } else {
                let mut lines = vec![
                    format!("| {} |", cells[0]),
                    format!(
                        "| {} |",
                        rows(&children[0]["content"]).iter().map(|cell|match cell.pointer("/content/0/attrs/textAlign").and_then(Value::as_str) {
                            Some("left")=>":---",Some("center")=>":---:",Some("right")=>"---:",_=>"---"
                        }).collect::<Vec<_>>().join(" | ")
                    ),
                ];
                lines.extend(cells[1..].iter().map(|r| format!("| {r} |")));
                lines.join("\n")
            }
        }
        _ => rendered("\n"),
    }
}

pub(crate) fn system_metadata_page(page: &Value, date: &str, start: i64, end: i64, subject: &str) -> Value {
    let generated = rows(&page["blocks"]).iter().any(|b| text(b,"id").starts_with("lesson-") && text(b,"id").ends_with("-goal-heading"))
        && rows(&page["blocks"]).iter().any(|b| text(b,"id").starts_with("lesson-") && text(b,"id").ends_with("-materials-heading"));
    if !generated || date.len() != 10 || !date.is_ascii() { return page.clone(); }
    let month = date[5..7].parse::<i64>().unwrap_or(0); let day = date[8..10].parse::<i64>().unwrap_or(0);
    let period = if start == end { format!("{start}") } else { format!("{start}~{end}") };
    let title = format!("{month}월 {day}일 {period}교시 · {subject}");
    let pattern = Regex::new(r"^\d{1,2}월 \d{1,2}일 \d{1,2}(?:~\d{1,2})?교시 · ").unwrap();
    let mut next = page.clone();
    let old_title = text(page,"title");
    if pattern.is_match(old_title) && old_title.ends_with(&format!(" · {subject}")) { next["title"] = json!(title); }
    let markdown = text(page,"markdown"); let first = markdown.lines().next().unwrap_or("");
    if first.starts_with("# ") && pattern.is_match(&first[2..]) && first.ends_with(&format!(" · {subject}")) {
        next["markdown"] = json!(format!("# {}{}",title,&markdown[first.len()..]));
    }
    if let Some(first) = next["blocks"].as_array_mut().and_then(|v| v.first_mut()) {
        if first["type"] == "h1" && pattern.is_match(text(first,"text")) && text(first,"text").ends_with(&format!(" · {subject}")) {
            first["text"] = json!(title);
        } else {
            let node = if first["content"].is_object() { &mut first["content"] } else { first };
            if node["type"] == "heading" && node["attrs"]["level"] == 1 && rows(&node["content"]).len() == 1
                && node["content"][0]["type"] == "text" && pattern.is_match(text(&node["content"][0],"text"))
                && text(&node["content"][0],"text").ends_with(&format!(" · {subject}")) { node["content"][0]["text"] = json!(title); }
        }
    }
    next
}

fn plan(page: &Value, data: &Value) -> Result<Value, String> {
    let a = &data["materialAuthority"];
    let aligned = system_metadata_page(page,text(a,"dateKey"),a["startPeriod"].as_i64().unwrap_or(0),a["endPeriod"].as_i64().unwrap_or(0),
        text(&data["transform"]["localBinding"],"subject"));
    let page = if snapshot(page) != data["baseSnapshot"] && !data["transform"]["localBinding"].is_null() { &aligned } else { page };
    if snapshot(page) != data["baseSnapshot"] {
        return Err("MATERIAL_REVISION_CONFLICT".into());
    }
    let target = &data["targetSnapshot"];
    let mut next = page.clone();
    if data["transform"]["strategy"] == "system_metadata" {
        let expected = system_metadata_page(page,text(a,"dateKey"),a["startPeriod"].as_i64().unwrap_or(0),a["endPeriod"].as_i64().unwrap_or(0),text(&data["transform"]["lesson"],"subject"));
        if snapshot(&expected) != *target { return Err(invalid()); }
        next = expected;
        next["title"] = target["documentTitle"].clone();
        let old_first = text(page,"markdown").lines().next().unwrap_or("");
        let new_first = text(target,"markdown").lines().next().unwrap_or("");
        if old_first.starts_with("# ") && new_first.starts_with("# ") {
            next["markdown"] = json!(format!("{}{}",new_first,&text(page,"markdown")[old_first.len()..]));
        }
    } else if matches!(
        text(&data["transform"], "strategy"),
        "organize" | "operations" | "replace_document"
    ) {
        let mut originals = HashMap::new();
        attachment_nodes(&page["blocks"], &mut originals);
        next["blocks"] = rehydrate(&target["documentBlocks"], &originals)?;
        next["markdown"] = json!(rows(&next["blocks"])
            .iter()
            .map(|block| node_markdown(
                if block["content"].is_object() {
                    &block["content"]
                } else {
                    block
                },
                0
            ))
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
            .trim());
        let next_text = format!("{}\n{}", text(&next, "markdown"), next["blocks"]);
        for found in Regex::new(r"https?://[^\s)\]<>]+")
            .unwrap()
            .find_iter(text(page, "markdown"))
        {
            if !next_text.contains(found.as_str()) {
                return Err("MATERIAL_REFERENCE_CONFLICT".into());
            }
        }
    } else {
        let mut index = 0;
        let mut blocks = Vec::new();
        for block in rows(&target["documentBlocks"]) {
            if let Some(asset) = rows(&data["attachments"])
                .iter()
                .find(|asset| block["type"] == "image" && asset["assetId"] == block["assetId"])
            {
                if asset["blockId"] != block["id"] {
                    return Err("MATERIAL_REFERENCE_CONFLICT".into());
                }
                blocks.push(json!({"id":asset["blockId"],"type":"attachment","text":block["alt"],"attachmentId":asset["assetId"],
                    "fileName":asset["fileName"],"contentType":asset["contentType"],"size":asset["size"],"kind":"image","displayMode":"preview"}));
            } else if rows(&data["appendBlocks"])
                .iter()
                .any(|candidate| candidate == block)
            {
                blocks.push(block.clone());
            } else {
                if data["baseSnapshot"]["documentBlocks"].get(index) != Some(block) {
                    return Err("MATERIAL_REFERENCE_CONFLICT".into());
                }
                blocks.push(page["blocks"][index].clone());
                index += 1;
            }
        }
        if index != rows(&page["blocks"]).len() {
            return Err("MATERIAL_REFERENCE_CONFLICT".into());
        }
        next["blocks"] = json!(blocks);
        next["markdown"] = json!([
            text(page, "markdown").trim_end(),
            text(data, "markdownDelta")
        ]
        .into_iter()
        .filter(|v| !v.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n"));
    }
    if snapshot(&next) != *target {
        return Err("MATERIAL_REFERENCE_CONFLICT".into());
    }
    next["updatedAtMs"] = json!(Utc::now()
        .timestamp_millis()
        .max(page["updatedAtMs"].as_i64().unwrap_or(0) + 1));
    Ok(next)
}

fn new_lesson_page(tenant: &str, data: &Value) -> Value {
    let a = &data["materialAuthority"];
    json!({"tenantId":tenant,"pageId":data["pageRef"],"parentId":format!("lesson-date-{}",text(a,"dateKey").replace("-","")),
      "title":data["targetSnapshot"]["documentTitle"],"emoji":"📖","position":a["startPeriod"],
      "properties":{"systemKind":"lesson_plan_binding"},"blocks":data["targetSnapshot"]["documentBlocks"],
      "markdown":data["targetSnapshot"]["markdown"],"createdAtMs":Utc::now().timestamp_millis(),"updatedAtMs":Utc::now().timestamp_millis()})
}
fn create_lesson_structure(conn: &Connection, tenant: &str, data: &Value, now: i64) -> Result<(), String> {
    let a = &data["materialAuthority"]; let date = text(a,"dateKey");
    let folder = format!("lesson-date-{}",date.replace("-",""));
    for (id,parent,title,kind) in [("lesson-materials-root",None,"수업자료","lesson_materials_folder"),
        (folder.as_str(),Some("lesson-materials-root"),date,"lesson_plan_date_projection")] {
        if (TransactionStore{conn}).get_work_note(tenant.into(),id.into())?.is_none() {
            crate::canonical_write_transactions::upsert_work_note(conn,json!({"tenantId":tenant,"pageId":id,"parentId":parent,"title":title,
              "emoji":"📚","position":0,"properties":{"systemKind":kind},"blocks":[],"markdown":"","createdAtMs":now,"updatedAtMs":now}))?;
        }
    }
    conn.execute("INSERT INTO lesson_plan_bindings(tenant_id,plan_id,page_id,plan_kind,date_key,start_period,end_period,subject,binding_revision,updated_at_ms) VALUES(?1,?2,?3,'lesson',?4,?5,?6,?7,1,?8)",
      params![tenant,text(a,"planId"),text(data,"pageRef"),date,a["startPeriod"].as_i64(),a["endPeriod"].as_i64(),text(&data["transform"]["lesson"],"subject"),now]).map_err(db)?;
    Ok(())
}

pub(super) fn apply(
    store: &SqliteStore,
    input: &Value,
    assets: &HashMap<String, Vec<u8>>,
) -> Result<Value, String> {
    let tenant = text(input, "tenantId");
    let data = &input["data"];
    let page_id = text(data, "pageRef");
    let create = data["transform"]["strategy"] == "create_local";
    let (page, mut next) = {
        let conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
        binding(&conn, tenant, data)?;
        let found = (TransactionStore { conn: &conn }).get_work_note(tenant.into(), page_id.into())?;
        let page = if create { Value::Null } else { found.ok_or("MATERIAL_REVISION_CONFLICT")? };
        let next = if create { new_lesson_page(tenant,data) } else { plan(&page, data)? };
        (page, next)
    };
    let mut manifest = json!({"mode":"append","workspace":"lesson_materials","pageRef":page_id,"expectedRevision":page["updatedAtMs"],
        "title":page["title"],"blocks":next["blocks"],"markdown":"","properties":{},"appendText":data["markdownDelta"],"attachments":data["attachments"]});
    manifest["contentSha256"] = json!(digest(
        &json!({"properties":{},"blocks":next["blocks"],"markdown":""})
    )?);
    let prepared = if rows(&data["attachments"]).is_empty() {
        vec![]
    } else {
        material_assets::validate(&manifest)?;
        material_assets::prepare(store, tenant, &manifest, assets)?
    };
    let mut conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
    let result = (|| {
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db)?;
        binding(&tx, tenant, data)?;
        let current = (TransactionStore { conn: &tx }).get_work_note(tenant.into(), page_id.into())?.unwrap_or(Value::Null);
        if current != page {
            return Err("MATERIAL_REVISION_CONFLICT".into());
        }
        if create { create_lesson_structure(&tx,tenant,data,next["updatedAtMs"].as_i64().ok_or_else(invalid)?)?; }
        let a = &data["materialAuthority"];
        let old_binding = &data["transform"]["localBinding"];
        if !old_binding.is_null() {
            tx.execute("UPDATE lesson_plan_bindings SET date_key=?1,start_period=?2,end_period=?3,binding_revision=?4,updated_at_ms=?5 WHERE tenant_id=?6 AND plan_id=?7 AND page_id=?8 AND binding_revision=?9",
                params![text(a,"dateKey"),a["startPeriod"].as_i64(),a["endPeriod"].as_i64(),a["bindingRevision"].as_i64(),Utc::now().timestamp_millis(),tenant,text(a,"planId"),page_id,old_binding["bindingRevision"].as_i64()]).map_err(db)?;
        }
        crate::lesson_plan_bindings::refresh_page_metadata(&tx,tenant,page_id,next["updatedAtMs"].as_i64().ok_or_else(invalid)?)?;
        if let Some((parent,_,position)) = crate::lesson_plan_bindings::stored_page_structure(&tx,tenant,page_id)? {
            next["parentId"] = json!(parent); next["position"] = json!(position);
        }
        crate::canonical_write_transactions::upsert_work_note(&tx, next.clone())?;
        let now = next["updatedAtMs"].as_i64().ok_or_else(invalid)?;
        for file in &prepared {
            let a = &file.asset;
            tx.execute("INSERT INTO work_note_attachments(tenant_id,attachment_id,page_id,block_id,file_name,content_type,byte_size,sha256,local_path,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?10)",
            params![tenant,text(a,"assetId"),page_id,text(a,"blockId"),text(a,"fileName"),text(a,"contentType"),a["size"].as_i64(),text(a,"sha256"),file.relative_path,now]).map_err(db)?;
        }
        let saved = TransactionStore { conn: &tx }
            .get_work_note(tenant.into(), page_id.into())?
            .ok_or("LOCAL_STORE_WRITE_FAILED")?;
        if saved != next || snapshot(&saved) != data["targetSnapshot"] {
            return Err("LOCAL_STORE_WRITE_FAILED".into());
        }
        let result = json!({"pageRef":page_id,"planId":data["materialAuthority"]["planId"],"snapshotRevision":data["snapshotRevision"],"contentSha256":data["contentSha256"]});
        let locator = json!({"operation":"lesson_material_apply_snapshot","recordId":page_id});
        let attachments: Vec<_> = rows(&data["attachments"])
            .iter()
            .map(|asset| {
                let mut asset = asset.clone();
                asset.as_object_mut().unwrap().remove("objectKey");
                asset.as_object_mut().unwrap().remove("stage");
                asset
            })
            .collect();
        let files = json!({"pageRef":page_id,"attachments":attachments});
        material_assets::verify_file_references(&tx, &store.data_dir, tenant, &files)?;
        let envelope = json!({"kind":receipt_verification::ENVELOPE,"result":result,"lessonAssets":files,
            "verification":{"locator":locator,"sha256":receipt_verification::digest(&tx,tenant,&locator)?}});
        let local_ref = format!("work-note-page:{page_id}");
        tx.execute(
            "INSERT INTO classaimate_mcp_local_write_receipts VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                tenant,
                text(input, "receiptId"),
                text(input, "operation"),
                text(input, "requestSha256"),
                envelope.to_string(),
                local_ref,
                now
            ],
        )
        .map_err(db)?;
        receipt_verification::verify(&tx, tenant, &envelope)?;
        tx.commit().map_err(db)?;
        receipt_verification::verify_after_commit(&conn, tenant, &envelope)?;
        material_assets::verify_file_references(&conn, &store.data_dir, tenant, &files)?;
        Ok(json!({"replayed":false,"result":result,"localRef":local_ref}))
    })();
    if result.is_err() {
        material_assets::cleanup(&conn, tenant, &prepared);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    fn wire(strategy: &str, images: bool) -> Value {
        // The main Node regression compares every bundled case to the real JS producer.
        // Public-builder tests only receive this desktop tree, not the API/DB or Node.
        let fixtures: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/classaimate-mcp-lesson-native-wire.json"
        ))
        .unwrap();
        assert_eq!(fixtures["version"], 1);
        assert_eq!(fixtures["synthetic"], true);
        let key = format!("{strategy}-{}", if images { "images" } else { "text" });
        fixtures["cases"]
            .get(&key)
            .expect("producer fixture case")
            .clone()
    }
    #[test]
    fn lesson_native_actual_producer_append_organize_operations_and_image_replay() {
        for (strategy, images) in [
            ("append", false),
            ("organize", false),
            ("operations", false),
            ("replace_document", false),
            ("append", true),
        ] {
            let wire = wire(strategy, images);
            let dir = std::env::temp_dir().join(format!(
                "classaimate-lesson-native-{}",
                crate::random_url_token()
            ));
            fs::create_dir_all(&dir).unwrap();
            let store = SqliteStore::open(dir.join("store.sqlite3")).unwrap();
            store.upsert_work_note(wire["page"].clone()).unwrap();
            crate::lesson_plan_bindings::upsert(
                &store,
                json!({"tenantId":"tenant-a","bindings":[wire["binding"].clone()]}),
            )
            .unwrap();
            let assets = rows(&wire["input"]["data"]["attachments"])
                .iter()
                .map(|asset| {
                    (
                        text(asset, "assetId").to_string(),
                        rows(&wire["png"])
                            .iter()
                            .map(|v| v.as_u64().unwrap() as u8)
                            .collect(),
                    )
                })
                .collect();
            let result = super::super::apply_with_assets(&store, &wire["input"], &assets)
                .unwrap_or_else(|e| panic!("{strategy}/{images}: {e}"));
            assert_eq!(result["result"]["snapshotRevision"], 2);
            let page = store
                .get_work_note("tenant-a".into(), "lesson-page".into())
                .unwrap()
                .unwrap();
            assert_eq!(page["properties"], wire["page"]["properties"]);
            assert!(text(&page, "markdown").contains("local-attachment://old-pdf"));
            assert_eq!(
                super::super::apply(&store, &wire["input"]).unwrap()["replayed"],
                true
            );
            drop(store);
            fs::remove_dir_all(dir).unwrap();
        }
    }
    #[test]
    fn lesson_native_binding_and_receipt_failures_roll_back_every_canonical_row() {
        for mode in ["binding", "receipt"] {
            let wire = wire("append", true);
            let dir = std::env::temp_dir().join(format!(
                "classaimate-lesson-rollback-{}",
                crate::random_url_token()
            ));
            fs::create_dir_all(&dir).unwrap();
            let store = SqliteStore::open(dir.join("store.sqlite3")).unwrap();
            store.upsert_work_note(wire["page"].clone()).unwrap();
            crate::lesson_plan_bindings::upsert(
                &store,
                json!({"tenantId":"tenant-a","bindings":[wire["binding"].clone()]}),
            )
            .unwrap();
            store.conn.lock().unwrap().execute_batch(if mode=="binding" {"UPDATE lesson_plan_bindings SET binding_revision=2"}
                else {"CREATE TRIGGER reject_lesson_receipt BEFORE INSERT ON classaimate_mcp_local_write_receipts BEGIN SELECT RAISE(ABORT,'receipt_failure'); END"}).unwrap();
            let bytes = rows(&wire["png"])
                .iter()
                .map(|v| v.as_u64().unwrap() as u8)
                .collect();
            let assets = HashMap::from([(
                text(&wire["input"]["data"]["attachments"][0], "assetId").into(),
                bytes,
            )]);
            assert!(super::super::apply_with_assets(&store, &wire["input"], &assets).is_err());
            assert_eq!(
                store
                    .get_work_note("tenant-a".into(), "lesson-page".into())
                    .unwrap()
                    .unwrap()["blocks"],
                wire["page"]["blocks"]
            );
            assert_eq!(
                store
                    .conn
                    .lock()
                    .unwrap()
                    .query_row("SELECT COUNT(*) FROM work_note_attachments", [], |r| r
                        .get::<_, i64>(0))
                    .unwrap(),
                0
            );
            drop(store);
            fs::remove_dir_all(dir).unwrap();
        }
    }
}
