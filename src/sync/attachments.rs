//! Attachment files [M3.3].
//!
//! Downloads are lazy by default (decision D5): a workspace of ten thousand
//! issues would otherwise pull gigabytes nobody asked for. Anything above the
//! threshold is represented by a small stub file recording what it is and how
//! to fetch it, so the folder still shows the truth about what the issue has.

use crate::api::{JiraApi, RemoteAttachment};
use crate::error::{JiredError, Result};
use crate::store::state::StateStore;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Suffix marking a placeholder for content that has not been downloaded.
pub const STUB_SUFFIX: &str = ".jiredstub";

/// What a stub file records, so `attachments pull` needs no other state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stub {
    pub id: String,
    pub filename: String,
    pub size: u64,
    pub mime: Option<String>,
    pub author: Option<String>,
    pub note: String,
}

impl Stub {
    fn of(a: &RemoteAttachment) -> Self {
        Self {
            id: a.id.clone(),
            filename: a.filename.clone(),
            size: a.size,
            mime: a.mime.clone(),
            author: a.author.clone(),
            note: "content not downloaded — run `jired attachments pull`".into(),
        }
    }
}

pub fn stub_path(dir: &Path, filename: &str) -> PathBuf {
    dir.join(format!("{filename}{STUB_SUFFIX}"))
}

/// Materialise an issue's attachment folder.
///
/// Small files are fetched eagerly because they are cheap and usually the ones
/// an agent wants; everything else becomes a stub.
pub fn sync_folder(
    state: &StateStore,
    client: &dyn JiraApi,
    issue_key: &str,
    dir: &Path,
    auto_download_max: u64,
    warnings: &mut Vec<String>,
) -> Result<usize> {
    let attachments = state.attachments(issue_key)?;
    if attachments.is_empty() {
        return Ok(0);
    }
    std::fs::create_dir_all(dir).map_err(crate::error::io_ctx("create", dir))?;

    let mut downloaded = 0;
    for a in &attachments {
        let real = dir.join(&a.filename);
        let stub = stub_path(dir, &a.filename);

        if real.exists() {
            continue;
        }
        if a.size <= auto_download_max {
            match download_one(state, client, issue_key, a, dir) {
                Ok(()) => {
                    let _ = std::fs::remove_file(&stub);
                    downloaded += 1;
                    continue;
                }
                // A failed download must not lose the record of the file — and
                // must not be a log line either: the user asked for this
                // attachment and needs to know it is not there.
                Err(e) => warnings.push(format!(
                    "{issue_key}: cannot download {}: {}",
                    a.filename, e.message
                )),
            }
        }
        if !stub.exists() {
            std::fs::write(
                &stub,
                format!("{}\n", serde_json::to_string_pretty(&Stub::of(a))?),
            )?;
        }
    }
    Ok(downloaded)
}

/// Fetch one attachment's content, verifying and recording its hash.
pub fn download_one(
    state: &StateStore,
    client: &dyn JiraApi,
    issue_key: &str,
    attachment: &RemoteAttachment,
    dir: &Path,
) -> Result<()> {
    let bytes = crate::api::block_on(client.download_attachment(attachment))?;
    if attachment.size > 0 && bytes.len() as u64 != attachment.size {
        return Err(JiredError::network(format!(
            "{}: the download was truncated (expected {} bytes, got {})",
            attachment.filename,
            attachment.size,
            bytes.len()
        ))
        .with_detail("filename", attachment.filename.clone())
        .with_hint("re-run `jired attachments pull` — a truncated download is usually transient"));
    }
    std::fs::create_dir_all(dir).map_err(crate::error::io_ctx("create", dir))?;
    std::fs::write(dir.join(&attachment.filename), &bytes)?;
    state.mark_attachment_downloaded(issue_key, &attachment.id, &sha256(&bytes))?;
    Ok(())
}

/// Files in the folder that the server does not have yet.
///
/// Anything the user drops in is an upload candidate; stubs and the files they
/// stand for are not.
pub fn pending_uploads(state: &StateStore, issue_key: &str, dir: &Path) -> Result<Vec<PathBuf>> {
    let known: std::collections::HashSet<String> = state
        .attachments(issue_key)?
        .into_iter()
        .map(|a| a.filename)
        .collect();

    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(Vec::new());
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .filter(|p| {
            let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
                return false;
            };
            !name.ends_with(STUB_SUFFIX) && !known.contains(name) && !name.starts_with('.')
        })
        .collect();
    out.sort();
    Ok(out)
}

pub fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

/// Refuse an upload that the server would reject anyway, with a clearer message.
pub fn check_upload_size(path: &Path, max: u64) -> Result<Vec<u8>> {
    let bytes = std::fs::read(path)
        .map_err(|e| JiredError::generic(format!("cannot read {}: {e}", path.display())))?;
    if bytes.len() as u64 > max {
        return Err(JiredError::usage(format!(
            "{} is {} but the limit is {}",
            path.display(),
            crate::sync::render::human_size(bytes.len() as u64),
            crate::sync::render::human_size(max)
        ))
        .with_detail("file", path.display().to_string())
        .with_detail("size", bytes.len() as u64)
        .with_detail("limit", max)
        .with_hint("raise attachments.max_upload if the server allows it"));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attachment(name: &str, size: u64) -> RemoteAttachment {
        RemoteAttachment {
            id: format!("id-{name}"),
            filename: name.to_string(),
            size,
            mime: Some("text/plain".into()),
            created: None,
            author: None,
            content_url: format!("https://fixture.invalid/{name}"),
        }
    }

    #[test]
    fn a_stub_records_enough_to_fetch_later() {
        let stub = Stub::of(&attachment("trace.har", 2_200_000));
        let text = serde_json::to_string(&stub).unwrap();
        let back: Stub = serde_json::from_str(&text).unwrap();
        assert_eq!(back.filename, "trace.har");
        assert_eq!(back.size, 2_200_000);
        assert!(back.note.contains("attachments pull"));
    }

    #[test]
    fn stub_paths_sit_beside_the_file_they_stand_for() {
        let dir = Path::new("/tmp/.PROJ-1. x");
        assert_eq!(
            stub_path(dir, "trace.har"),
            Path::new("/tmp/.PROJ-1. x/trace.har.jiredstub")
        );
    }

    #[test]
    fn pending_uploads_ignores_stubs_and_known_files() {
        let state = StateStore::in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();

        std::fs::write(dir.path().join("known.txt"), b"x").unwrap();
        std::fs::write(dir.path().join("new-file.txt"), b"y").unwrap();
        std::fs::write(dir.path().join("big.bin.jiredstub"), b"{}").unwrap();
        std::fs::write(dir.path().join(".DS_Store"), b"junk").unwrap();

        // Record `known.txt` as already on the server.
        let issue = crate::api::RemoteIssue {
            key: "P-1".into(),
            id: "1".into(),
            updated: crate::api::model::parse_timestamp("2026-01-01T00:00:00Z").unwrap(),
            description: crate::api::RichText::Empty,
            fields: Default::default(),
            comments: vec![],
            comment_total: 0,
            worklogs: Vec::new(),
            worklog_total: 0,
            attachments: vec![attachment("known.txt", 1)],
            raw: serde_json::json!({
                "key": "P-1", "id": "1",
                "fields": {"updated": "2026-01-01T00:00:00Z"}
            }),
        };
        state.upsert_remote(&issue).unwrap();

        let pending = pending_uploads(&state, "P-1", dir.path()).unwrap();
        let names: Vec<String> = pending
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["new-file.txt"]);
    }

    #[test]
    fn a_missing_folder_has_nothing_to_upload() {
        let state = StateStore::in_memory().unwrap();
        assert!(pending_uploads(&state, "P-1", Path::new("/nope/nowhere"))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn an_oversized_upload_is_refused_before_the_request() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.bin");
        std::fs::write(&path, vec![0u8; 2048]).unwrap();

        let err = check_upload_size(&path, 1024).unwrap_err();
        assert_eq!(err.exit_code().as_i32(), 2);
        assert_eq!(err.details["size"], 2048);
        assert_eq!(err.details["limit"], 1024);

        assert_eq!(check_upload_size(&path, 4096).unwrap().len(), 2048);
    }

    #[test]
    fn hashing_is_stable() {
        assert_eq!(sha256(b"abc"), sha256(b"abc"));
        assert_ne!(sha256(b"abc"), sha256(b"abd"));
        assert_eq!(sha256(b"").len(), 64);
    }
}
