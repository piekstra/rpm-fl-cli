//! `rpmfl documents` — shared documents (1099s, W-9s, management agreements)
//! and owner statement packets.
//!
//! The portal returns pre-signed S3 URLs that expire in ~5 minutes, so `get`
//! fetches immediately rather than printing a link that will be dead by the
//! time anyone clicks it. `--url` prints it anyway for piping.

use std::collections::HashMap;
use std::io::Write;

use clap::{Args, Subcommand};
use pk_cli_core::{output, CliError};
use pk_cli_documents::{Document, Paged, SavedDocument};
use serde_json::{json, Value};

use super::{table_view, Ctx};

#[derive(Args, Debug)]
pub struct GetArgs {
    /// Document ID (see `rpmfl documents list`).
    pub document_id: String,

    /// Write to this path (default: the document's own filename).
    #[arg(long, short)]
    pub output: Option<String>,

    /// Print the pre-signed URL instead of downloading. Expires in ~5 minutes.
    #[arg(long)]
    pub url: bool,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// List every shared document.
    #[command(visible_alias = "ls")]
    List,
    /// Download one document by ID.
    Get(GetArgs),
}

pub fn run(ctx: &Ctx, cmd: &Cmd) -> Result<(), CliError> {
    let client = ctx.client()?;
    let payload = client.get("/oportal/api/shared_documents/", &[])?;
    let docs = collect(&payload);

    match cmd {
        Cmd::List => {
            let env = document_list_json(&docs);
            if ctx.common.json {
                output::json(&env);
            } else {
                let rows = env
                    .get("items")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                output::table(&table_view(
                    &rows,
                    &["id", "name", "category", "date", "size"],
                ));
            }
            Ok(())
        }
        Cmd::Get(args) => {
            let raw = docs
                .iter()
                .find(|d| d.get("id").map(scalar_id) == Some(args.document_id.clone()))
                .ok_or_else(|| {
                    CliError::NotFound(format!("no document with id {}", args.document_id))
                })?;
            let url = raw
                .get("download_url")
                .and_then(Value::as_str)
                .ok_or_else(|| CliError::Upstream("document has no download URL".into()))?;

            if args.url {
                println!("{url}");
                return Ok(());
            }

            let doc = document_of(raw);
            let path = match &args.output {
                // An explicit `-o` target is the operator's own choice.
                Some(o) => o.clone(),
                // The default name comes from the portal, where other parties
                // on the account (property manager, co-owners) control it — so
                // reduce it to a bare leaf that can't traverse out of the cwd.
                None => doc
                    .file
                    .as_deref()
                    .map(std::path::Path::new)
                    .and_then(std::path::Path::file_name)
                    .map(|s| s.to_string_lossy().into_owned())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "document.pdf".to_string()),
            };
            let bytes = client.download(url)?;
            std::fs::File::create(&path)
                .and_then(|mut f| f.write_all(&bytes))
                .map_err(|e| CliError::Other(format!("writing {path}: {e}")))?;
            if !ctx.common.quiet {
                eprintln!("wrote {} ({} bytes)", path, bytes.len());
            }
            if ctx.common.json {
                // Full `document-download/v1`: the listed document's id/name/
                // category/date carried through to what landed on disk.
                let saved = SavedDocument::from_document(&doc, path, bytes.len() as u64);
                output::json(
                    &serde_json::to_value(saved).expect("document-download/v1 serializes"),
                );
            }
            Ok(())
        }
    }
}

/// The `document-list/v1` envelope: the documents/v1 [`Document`] items, plus
/// rpmfl's provider extras (`shared_at`/`size`/`content_type`/`folder_name`)
/// folded back onto each item beside the profile fields — a documents/v1
/// consumer reads the known keys and ignores the rest (fpl/wabhoa pattern).
///
/// Extras are matched to their item **by id**, not by position, so a future
/// cli-common that reorders/filters/paginates `Paged` items can only ever
/// produce *missing* extras, never extras misattributed to another document.
/// Pure (no `Ctx`/HTTP) so it is unit-tested directly.
fn document_list_json(docs: &[Value]) -> Value {
    let items: Vec<Document> = docs.iter().map(document_of).collect();
    let mut env =
        serde_json::to_value(Paged::new("document", items)).expect("document-list/v1 serializes");

    let by_id: HashMap<String, &Value> = docs
        .iter()
        .filter_map(|d| d.get("id").map(|v| (scalar_id(v), d)))
        .collect();
    if let Some(arr) = env.get_mut("items").and_then(Value::as_array_mut) {
        for item in arr {
            let Some(obj) = item.as_object_mut() else {
                continue;
            };
            let id = match obj.get("id").and_then(Value::as_str) {
                Some(s) => s.to_string(),
                None => continue,
            };
            let Some(raw) = by_id.get(id.as_str()) else {
                continue;
            };
            for key in ["shared_at", "size", "content_type", "folder_name"] {
                match raw.get(key) {
                    Some(v) if !v.is_null() => {
                        obj.insert(key.to_string(), v.clone());
                    }
                    _ => {}
                }
            }
        }
    }
    env
}

/// Map one raw portal document (as flattened by [`collect`]) onto a
/// documents/v1 [`Document`]. The portal's `name` is the filename, so it also
/// serves as `file` (the default save name). No financial fields — a document
/// is just what an archiver needs to file and fetch it.
fn document_of(raw: &Value) -> Document {
    let id = raw.get("id").map(scalar_id).unwrap_or_default();
    let name = raw
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mut d = Document::new(id, name.clone());
    d.category = raw
        .get("category")
        .and_then(Value::as_str)
        .map(str::to_string);
    d.date = raw
        .get("shared_at")
        .and_then(Value::as_str)
        .and_then(crate::dates::date_from_portal_timestamp);
    if !name.is_empty() {
        d.file = Some(name);
    }
    d
}

/// The endpoint groups documents by owner and by kind; flatten them into one
/// list, tagging each with the category it came from.
fn collect(payload: &Value) -> Vec<Value> {
    const GROUPS: [(&str, &str); 5] = [
        ("documents", "shared"),
        ("management_agreement_documents", "management-agreement"),
        ("uploaded_pdf_management_agreements", "management-agreement"),
        ("pdf_template_management_agreements", "management-agreement"),
        ("insurance_policy_documents", "insurance"),
    ];

    let owners = payload.as_array().cloned().unwrap_or_default();
    let mut out = Vec::new();
    for owner in &owners {
        for (key, category) in GROUPS {
            let Some(items) = owner.get(key).and_then(Value::as_array) else {
                continue;
            };
            for d in items {
                out.push(json!({
                    "id": d.get("id").cloned().unwrap_or(Value::Null),
                    "name": d.get("name").cloned().unwrap_or(Value::Null),
                    "category": category,
                    // Uploaded docs use `shared_at`; generated ones use `sent_at`.
                    "shared_at": d.get("shared_at").or_else(|| d.get("sent_at")).cloned().unwrap_or(Value::Null),
                    "size": d.get("size").cloned().unwrap_or(Value::Null),
                    "content_type": d.get("content_type").cloned().unwrap_or(Value::Null),
                    "folder_name": d.get("folder_name").cloned().unwrap_or(Value::Null),
                    "download_url": d.get("download_url").cloned().unwrap_or(Value::Null),
                }));
            }
        }
    }
    out
}

/// Document IDs arrive as numbers; compare them as strings.
fn scalar_id(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_flattens_every_group() {
        let payload = json!([{
            "id": 1, "name": "Owner",
            "documents": [{ "id": 10, "name": "1099.pdf", "shared_at": "2026/01/18 18:30:18 -0500", "size": 100 }],
            "pdf_template_management_agreements": [{ "id": 20, "name": "Agreement", "sent_at": "2025/04/08 10:35:04 -0400" }],
            "insurance_policy_documents": [],
        }]);
        let docs = collect(&payload);
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0]["category"], "shared");
        assert_eq!(docs[0]["shared_at"], "2026/01/18 18:30:18 -0500");
        // `sent_at` stands in for `shared_at` on generated documents.
        assert_eq!(docs[1]["category"], "management-agreement");
        assert_eq!(docs[1]["shared_at"], "2025/04/08 10:35:04 -0400");
    }

    #[test]
    fn collect_tolerates_empty_payload() {
        assert!(collect(&json!([])).is_empty());
        assert!(collect(&json!({})).is_empty());
    }

    #[test]
    fn ids_compare_as_strings() {
        assert_eq!(scalar_id(&json!(42)), "42");
        assert_eq!(scalar_id(&json!("42")), "42");
    }

    #[test]
    fn document_of_conforms_to_documents_v1() {
        let raw = collect(&json!([{
            "id": 1, "name": "Owner",
            "documents": [{ "id": 10, "name": "1099.pdf", "shared_at": "2026/01/18 18:30:18 -0500", "size": 100 }],
        }]))[0]
            .clone();
        let v = serde_json::to_value(document_of(&raw)).unwrap();
        assert_eq!(v["id"], "10"); // numeric portal id → string
        assert_eq!(v["name"], "1099.pdf");
        assert_eq!(v["category"], "shared");
        assert_eq!(v["date"], "2026-01-18"); // timestamp → ISO date portion
        assert_eq!(v["file"], "1099.pdf"); // name doubles as the save filename
        assert!(
            v.get("amount").is_none(),
            "no financial fields on a document"
        );
        // Round-trips through the profile type.
        let _: Document = serde_json::from_value(v).unwrap();
    }

    #[test]
    fn document_list_json_is_the_profile_envelope_with_extras() {
        let docs = collect(&json!([{
            "id": 1, "name": "Owner",
            "documents": [{
                "id": 10, "name": "1099.pdf", "shared_at": "2026/01/18 18:30:18 -0500",
                "size": 100, "content_type": "application/pdf"
            }],
        }]));
        let env = document_list_json(&docs);
        assert_eq!(env["schema"], "document-list/v1");
        assert_eq!(env["items"].as_array().unwrap().len(), 1);
        let item = &env["items"][0];
        // Profile fields…
        assert_eq!(item["id"], "10");
        assert_eq!(item["date"], "2026-01-18");
        // …plus provider extras folded on, nulls omitted (folder_name absent).
        assert_eq!(item["size"], 100);
        assert_eq!(item["content_type"], "application/pdf");
        assert!(item.get("folder_name").is_none());
    }

    #[test]
    fn document_list_extras_attach_to_their_own_document_by_id() {
        // Each item must carry its OWN size, matched by id — not by position.
        let docs = vec![
            json!({ "id": 10, "name": "a.pdf", "size": 11 }),
            json!({ "id": 20, "name": "b.pdf", "size": 22 }),
        ];
        let env = document_list_json(&docs);
        for it in env["items"].as_array().unwrap() {
            let expected = if it["id"] == "10" { 11 } else { 22 };
            assert_eq!(it["size"], expected, "id {} got the wrong size", it["id"]);
        }
    }
}
