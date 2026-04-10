use std::collections::HashMap;
use std::collections::HashSet;
use std::fs::File;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::path::Path;
use std::path::PathBuf;

use codex_protocol::ThreadId;
use codex_protocol::protocol::SessionMetaLine;
use serde::Deserialize;
use serde::Serialize;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;

const SESSION_INDEX_FILE: &str = "session_index.jsonl";
const READ_CHUNK_SIZE: usize = 8192;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionIndexEntry {
    pub id: ThreadId,
    pub thread_name: String,
    pub updated_at: String,
}

/// Append a thread name update to the session index.
/// The index is append-only; readers prefer newer entries when resolving names or ids.
pub async fn append_thread_name(
    codex_home: &Path,
    thread_id: ThreadId,
    name: &str,
) -> std::io::Result<()> {
    use time::OffsetDateTime;
    use time::format_description::well_known::Rfc3339;

    let updated_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown".to_string());
    let entry = SessionIndexEntry {
        id: thread_id,
        thread_name: name.to_string(),
        updated_at,
    };
    append_session_index_entry(codex_home, &entry).await
}

/// Append a raw session index entry to `session_index.jsonl`.
/// The file is append-only; consumers generally scan from the end to prefer newer matches.
pub async fn append_session_index_entry(
    codex_home: &Path,
    entry: &SessionIndexEntry,
) -> std::io::Result<()> {
    let path = session_index_path(codex_home);
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await?;
    let mut line = serde_json::to_string(entry).map_err(std::io::Error::other)?;
    line.push('\n');
    file.write_all(line.as_bytes()).await?;
    file.flush().await?;
    Ok(())
}

/// Find the latest thread name for a thread id, if any.
pub async fn find_thread_name_by_id(
    codex_home: &Path,
    thread_id: &ThreadId,
) -> std::io::Result<Option<String>> {
    let path = session_index_path(codex_home);
    if !path.exists() {
        return Ok(None);
    }
    let id = *thread_id;
    let entry = tokio::task::spawn_blocking(move || scan_index_from_end_by_id(&path, &id))
        .await
        .map_err(std::io::Error::other)??;
    Ok(entry.map(|entry| entry.thread_name))
}

/// Find the latest thread names for a batch of thread ids.
pub async fn find_thread_names_by_ids(
    codex_home: &Path,
    thread_ids: &HashSet<ThreadId>,
) -> std::io::Result<HashMap<ThreadId, String>> {
    let path = session_index_path(codex_home);
    if thread_ids.is_empty() || !path.exists() {
        return Ok(HashMap::new());
    }

    let file = tokio::fs::File::open(&path).await?;
    let reader = tokio::io::BufReader::new(file);
    let mut lines = reader.lines();
    let mut names = HashMap::with_capacity(thread_ids.len());

    while let Some(line) = lines.next_line().await? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<SessionIndexEntry>(trimmed) else {
            continue;
        };
        let name = entry.thread_name.trim();
        if !name.is_empty() && thread_ids.contains(&entry.id) {
            names.insert(entry.id, name.to_string());
        }
    }

    Ok(names)
}

/// Locate a recorded thread rollout and read its session metadata by thread name.
/// Returns the newest indexed name that still has a readable rollout header.
pub async fn find_thread_meta_by_name_str(
    codex_home: &Path,
    name: &str,
) -> std::io::Result<Option<(PathBuf, SessionMetaLine)>> {
    let name = name.trim();
    if name.is_empty() {
        return Ok(None);
    }
    let path = session_index_path(codex_home);
    if !path.exists() {
        return Ok(None);
    }

    let name = name.to_string();
    let thread_ids =
        tokio::task::spawn_blocking(move || scan_index_from_end_thread_ids_by_name(&path, &name))
            .await
            .map_err(std::io::Error::other)??;

    for thread_id in thread_ids {
        if let Some(path) =
            super::list::find_thread_path_by_id_str(codex_home, &thread_id.to_string()).await?
            && let Ok(session_meta) = super::list::read_session_meta_line(&path).await
        {
            return Ok(Some((path, session_meta)));
        }
    }

    Ok(None)
}

/// Locate a recorded thread rollout file by thread name, preferring newer entries first
/// and skipping names that do not yet resolve to saved rollout data.
/// Returns `Ok(Some(path))` if found, `Ok(None)` if not present.
#[cfg_attr(not(test), allow(dead_code))]
pub async fn find_thread_path_by_name_str(
    codex_home: &Path,
    name: &str,
) -> std::io::Result<Option<PathBuf>> {
    let name = name.trim();
    if name.is_empty() {
        return Ok(None);
    }
    let path = session_index_path(codex_home);
    if !path.exists() {
        return Ok(None);
    }

    let name = name.to_string();
    let thread_ids =
        tokio::task::spawn_blocking(move || scan_index_from_end_thread_ids_by_name(&path, &name))
            .await
            .map_err(std::io::Error::other)??;

    for thread_id in thread_ids {
        if let Some(path) =
            super::list::find_thread_path_by_id_str(codex_home, &thread_id.to_string()).await?
        {
            return Ok(Some(path));
        }
    }

    Ok(None)
}

fn session_index_path(codex_home: &Path) -> PathBuf {
    codex_home.join(SESSION_INDEX_FILE)
}

fn scan_index_from_end_by_id(
    path: &Path,
    thread_id: &ThreadId,
) -> std::io::Result<Option<SessionIndexEntry>> {
    scan_index_from_end(path, |entry| entry.id == *thread_id)
}

fn scan_index_from_end_thread_ids_by_name(
    path: &Path,
    name: &str,
) -> std::io::Result<Vec<ThreadId>> {
    let mut file = File::open(path)?;
    let mut remaining = file.metadata()?.len();
    let mut line_rev: Vec<u8> = Vec::new();
    let mut buf = vec![0u8; READ_CHUNK_SIZE];
    let mut thread_ids = Vec::new();
    let mut seen = HashSet::new();

    while remaining > 0 {
        let read_size = usize::try_from(remaining.min(READ_CHUNK_SIZE as u64))
            .map_err(std::io::Error::other)?;
        remaining -= read_size as u64;
        file.seek(SeekFrom::Start(remaining))?;
        file.read_exact(&mut buf[..read_size])?;

        for &byte in buf[..read_size].iter().rev() {
            if byte == b'\n' {
                collect_thread_id_from_rev(&mut line_rev, name, &mut seen, &mut thread_ids)?;
                continue;
            }
            line_rev.push(byte);
        }
    }

    collect_thread_id_from_rev(&mut line_rev, name, &mut seen, &mut thread_ids)?;

    Ok(thread_ids)
}

fn scan_index_from_end<F>(
    path: &Path,
    mut predicate: F,
) -> std::io::Result<Option<SessionIndexEntry>>
where
    F: FnMut(&SessionIndexEntry) -> bool,
{
    scan_index_from_end_for_each(path, |entry| {
        if predicate(entry) {
            return Ok(Some(entry.clone()));
        }
        Ok(None)
    })
}

fn scan_index_from_end_for_each<F>(
    path: &Path,
    mut visit_entry: F,
) -> std::io::Result<Option<SessionIndexEntry>>
where
    F: FnMut(&SessionIndexEntry) -> std::io::Result<Option<SessionIndexEntry>>,
{
    let mut file = File::open(path)?;
    let mut remaining = file.metadata()?.len();
    let mut line_rev: Vec<u8> = Vec::new();
    let mut buf = vec![0u8; READ_CHUNK_SIZE];

    while remaining > 0 {
        let read_size = usize::try_from(remaining.min(READ_CHUNK_SIZE as u64))
            .map_err(std::io::Error::other)?;
        remaining -= read_size as u64;
        file.seek(SeekFrom::Start(remaining))?;
        file.read_exact(&mut buf[..read_size])?;

        for &byte in buf[..read_size].iter().rev() {
            if byte == b'\n' {
                if let Some(entry) = parse_line_from_rev(&mut line_rev, &mut visit_entry)? {
                    return Ok(Some(entry));
                }
                continue;
            }
            line_rev.push(byte);
        }
    }

    if let Some(entry) = parse_line_from_rev(&mut line_rev, &mut visit_entry)? {
        return Ok(Some(entry));
    }

    Ok(None)
}

fn collect_thread_id_from_rev(
    line_rev: &mut Vec<u8>,
    name: &str,
    seen: &mut HashSet<ThreadId>,
    thread_ids: &mut Vec<ThreadId>,
) -> std::io::Result<()> {
    let Some(entry) = parse_line_from_rev(line_rev, &mut |entry| {
        Ok((entry.thread_name == name).then_some(entry.clone()))
    })?
    else {
        return Ok(());
    };
    if seen.insert(entry.id) {
        thread_ids.push(entry.id);
    }
    Ok(())
}

fn parse_line_from_rev<F>(
    line_rev: &mut Vec<u8>,
    visit_entry: &mut F,
) -> std::io::Result<Option<SessionIndexEntry>>
where
    F: FnMut(&SessionIndexEntry) -> std::io::Result<Option<SessionIndexEntry>>,
{
    if line_rev.is_empty() {
        return Ok(None);
    }
    line_rev.reverse();
    let line = std::mem::take(line_rev);
    let Ok(mut line) = String::from_utf8(line) else {
        return Ok(None);
    };
    if line.ends_with('\r') {
        line.pop();
    }
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let Ok(entry) = serde_json::from_str::<SessionIndexEntry>(trimmed) else {
        return Ok(None);
    };
    visit_entry(&entry)
}

#[cfg(test)]
#[path = "session_index_tests.rs"]
mod tests;
