//! `rpmfl documents` — shared documents (1099s, W-9s, management agreements)
//! and owner statement packets.
//!
//! The portal returns pre-signed S3 URLs that expire in ~5 minutes, so `get`
//! fetches immediately rather than printing a link that will be dead by the
//! time anyone clicks it. `--url` prints it anyway for piping.

use std::io::Write;

use clap::{Args, Subcommand};
use pk_cli_core::{output, CliError};
use serde_json::{json, Value};

use super::{emit, table_view, Ctx};

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
            emit(
                ctx,
                "document-list",
                json!({ "count": docs.len(), "documents": docs }),
                |v| {
                    let rows = v
                        .get("documents")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    output::table(&table_view(
                        &rows,
                        &["id", "name", "category", "shared_at", "size"],
                    ));
                },
            );
            Ok(())
        }
        Cmd::Get(args) => {
            let doc = docs
                .iter()
                .find(|d| d.get("id").map(scalar_id) == Some(args.document_id.clone()))
                .ok_or_else(|| {
                    CliError::NotFound(format!("no document with id {}", args.document_id))
                })?;
            let url = doc
                .get("download_url")
                .and_then(Value::as_str)
                .ok_or_else(|| CliError::Upstream("document has no download URL".into()))?;

            if args.url {
                println!("{url}");
                return Ok(());
            }

            let name = doc
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("document.pdf");
            let path = args.output.clone().unwrap_or_else(|| name.to_string());
            let bytes = client.download(url)?;
            std::fs::File::create(&path)
                .and_then(|mut f| f.write_all(&bytes))
                .map_err(|e| CliError::Other(format!("writing {path}: {e}")))?;
            if !ctx.common.quiet {
                eprintln!("wrote {} ({} bytes)", path, bytes.len());
            }
            if ctx.common.json {
                output::json(&json!({
                    "schema": "document-download/v1",
                    "path": path,
                    "bytes": bytes.len(),
                }));
            }
            Ok(())
        }
    }
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
}
