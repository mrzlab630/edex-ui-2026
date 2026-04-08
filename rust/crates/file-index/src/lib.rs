use core_policy::Sensitivity;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    pub name: String,
    pub kind: FileKind,
    pub size_bytes: Option<u64>,
    pub modified_unix_ms: Option<u64>,
    pub hidden: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FilePreview {
    pub path: String,
    pub content: Option<String>,
    pub truncated: bool,
    pub is_text: bool,
    pub sensitivity: Sensitivity,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileSearchResult {
    pub entry: FileEntry,
    pub score: u32,
}

#[derive(Debug, Clone, Default)]
pub struct FileIndex {
    roots: HashMap<PathBuf, IndexedRoot>,
}

#[derive(Debug, Clone, Default)]
struct IndexedRoot {
    entries: HashMap<PathBuf, FileEntry>,
    children: HashMap<PathBuf, Vec<PathBuf>>,
    inverted_index: HashMap<String, HashSet<PathBuf>>,
}

#[derive(Debug, thiserror::Error)]
pub enum FileIndexError {
    #[error("file index io error: {0}")]
    Io(String),
    #[error("path `{0}` is not a directory")]
    NotDirectory(String),
    #[error("root `{0}` is not indexed")]
    RootNotIndexed(String),
    #[error("path `{0}` is not indexed and does not exist")]
    PathNotFound(String),
    #[error("root `{root}` exceeds entry limit {max_entries}")]
    EntryLimitExceeded { root: String, max_entries: usize },
    #[error("file index access denied: {0}")]
    AccessDenied(String),
}

impl FileIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn refresh_root(&mut self, root_path: impl AsRef<Path>) -> Result<usize, FileIndexError> {
        self.refresh_root_with_limit(root_path, usize::MAX)
    }

    pub fn refresh_root_with_limit(
        &mut self,
        root_path: impl AsRef<Path>,
        max_entries: usize,
    ) -> Result<usize, FileIndexError> {
        let root_path = canonicalize(root_path.as_ref())?;
        if !root_path.is_dir() {
            return Err(FileIndexError::NotDirectory(
                root_path.display().to_string(),
            ));
        }

        let mut indexed = IndexedRoot::default();
        let mut stack = vec![root_path.clone()];

        while let Some(directory) = stack.pop() {
            let mut child_paths = Vec::new();
            for child in read_dir_sorted(&directory)? {
                let child_path = child.path();
                let metadata = child.metadata().map_err(io_error)?;
                if indexed.entries.len() >= max_entries {
                    return Err(FileIndexError::EntryLimitExceeded {
                        root: root_path.display().to_string(),
                        max_entries,
                    });
                }
                let entry = file_entry_for_path(&child_path, &metadata);
                index_tokens(&mut indexed.inverted_index, &entry, &child_path);
                indexed.entries.insert(child_path.clone(), entry);
                child_paths.push(child_path.clone());

                if metadata.is_dir() {
                    stack.push(child_path);
                }
            }

            child_paths.sort();
            indexed.children.insert(directory, child_paths);
        }

        let count = indexed.entries.len();
        self.roots.insert(root_path, indexed);
        Ok(count)
    }

    pub fn indexed_count(&self, root_path: impl AsRef<Path>) -> Result<usize, FileIndexError> {
        let root_path = canonicalize(root_path.as_ref())?;
        let root = self
            .roots
            .get(&root_path)
            .ok_or_else(|| FileIndexError::RootNotIndexed(root_path.display().to_string()))?;
        Ok(root.entries.len())
    }

    pub fn root_count(&self) -> usize {
        self.roots.len()
    }

    pub fn contains_root(&self, root_path: impl AsRef<Path>) -> Result<bool, FileIndexError> {
        let root_path = canonicalize(root_path.as_ref())?;
        Ok(self.roots.contains_key(&root_path))
    }

    pub fn list_dir(
        &self,
        directory: impl AsRef<Path>,
        limit: usize,
    ) -> Result<Vec<FileEntry>, FileIndexError> {
        let directory = canonicalize(directory.as_ref())?;
        if let Some(root) = self.root_for_path(&directory) {
            if let Some(children) = root.children.get(&directory) {
                let mut entries: Vec<_> = children
                    .iter()
                    .filter_map(|path| root.entries.get(path).cloned())
                    .collect();
                sort_entries(&mut entries);
                entries.truncate(limit.max(1));
                return Ok(entries);
            }
        }

        if !directory.exists() {
            return Err(FileIndexError::PathNotFound(
                directory.display().to_string(),
            ));
        }
        if !directory.is_dir() {
            return Err(FileIndexError::NotDirectory(
                directory.display().to_string(),
            ));
        }

        let mut entries = Vec::new();
        for child in read_dir_sorted(&directory)? {
            let child_path = child.path();
            let metadata = child.metadata().map_err(io_error)?;
            entries.push(file_entry_for_path(&child_path, &metadata));
        }
        sort_entries(&mut entries);
        entries.truncate(limit.max(1));
        Ok(entries)
    }

    pub fn stat_path(&self, path: impl AsRef<Path>) -> Result<FileEntry, FileIndexError> {
        let path = canonicalize(path.as_ref())?;
        if let Some(root) = self.root_for_path(&path) {
            if let Some(entry) = root.entries.get(&path) {
                return Ok(entry.clone());
            }
        }

        let metadata = fs::symlink_metadata(&path).map_err(io_error)?;
        Ok(file_entry_for_path(&path, &metadata))
    }

    pub fn preview_file(
        &self,
        path: impl AsRef<Path>,
        max_bytes: usize,
        max_lines: usize,
    ) -> Result<FilePreview, FileIndexError> {
        let path = canonicalize(path.as_ref())?;
        let metadata = fs::metadata(&path).map_err(io_error)?;
        let size_bytes = metadata.len();
        let path_string = path.display().to_string();

        if metadata.is_dir() || is_sensitive_path(&path) {
            return Ok(FilePreview {
                path: path_string,
                content: None,
                truncated: false,
                is_text: false,
                sensitivity: Sensitivity::SensitiveRawContent,
                size_bytes,
            });
        }

        let bytes = fs::read(&path).map_err(io_error)?;
        let truncated = bytes.len() > max_bytes;
        let bytes = &bytes[..bytes.len().min(max_bytes.max(1))];
        let Ok(text) = String::from_utf8(bytes.to_vec()) else {
            return Ok(FilePreview {
                path: path_string,
                content: None,
                truncated,
                is_text: false,
                sensitivity: Sensitivity::SafeMetadata,
                size_bytes,
            });
        };

        let lines: Vec<_> = text.lines().take(max_lines.max(1)).collect();
        let line_truncated = text.lines().count() > max_lines.max(1);
        let preview = lines.join("\n");

        Ok(FilePreview {
            path: path.display().to_string(),
            content: Some(preview),
            truncated: truncated || line_truncated,
            is_text: true,
            sensitivity: Sensitivity::RedactableContent,
            size_bytes,
        })
    }

    pub fn search(
        &self,
        root_path: impl AsRef<Path>,
        text: &str,
        limit: usize,
    ) -> Result<Vec<FileSearchResult>, FileIndexError> {
        let root_path = canonicalize(root_path.as_ref())?;
        let root = self
            .roots
            .get(&root_path)
            .ok_or_else(|| FileIndexError::RootNotIndexed(root_path.display().to_string()))?;
        let tokens = tokenize(text);
        if tokens.is_empty() {
            return Ok(Vec::new());
        }

        let mut scores: HashMap<PathBuf, u32> = HashMap::new();
        for token in tokens {
            if let Some(paths) = root.inverted_index.get(&token) {
                for path in paths {
                    *scores.entry(path.clone()).or_default() += 1;
                }
            }
        }

        let mut results: Vec<_> = scores
            .into_iter()
            .filter_map(|(path, score)| {
                root.entries
                    .get(&path)
                    .cloned()
                    .map(|entry| FileSearchResult { entry, score })
            })
            .collect();

        results.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then(left.entry.path.cmp(&right.entry.path))
        });
        results.truncate(limit.max(1));
        Ok(results)
    }
}

fn file_entry_for_path(path: &Path, metadata: &fs::Metadata) -> FileEntry {
    FileEntry {
        path: path.display().to_string(),
        name: path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string()),
        kind: if metadata.is_dir() {
            FileKind::Directory
        } else if metadata.is_file() {
            FileKind::File
        } else if metadata.file_type().is_symlink() {
            FileKind::Symlink
        } else {
            FileKind::Other
        },
        size_bytes: metadata.is_file().then_some(metadata.len()),
        modified_unix_ms: metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64),
        hidden: path
            .file_name()
            .map(|name| name.to_string_lossy().starts_with('.'))
            .unwrap_or(false),
    }
}

fn sort_entries(entries: &mut [FileEntry]) {
    entries.sort_by(|left, right| {
        match (
            left.kind == FileKind::Directory,
            right.kind == FileKind::Directory,
        ) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => left.name.cmp(&right.name),
        }
    });
}

fn index_tokens(
    inverted_index: &mut HashMap<String, HashSet<PathBuf>>,
    entry: &FileEntry,
    path: &Path,
) {
    for token in tokenize(&entry.name)
        .into_iter()
        .chain(tokenize(&path.display().to_string()))
    {
        inverted_index
            .entry(token)
            .or_default()
            .insert(path.to_path_buf());
    }
}

fn tokenize(input: &str) -> Vec<String> {
    input
        .split(|ch: char| !ch.is_alphanumeric())
        .map(|token| token.trim().to_ascii_lowercase())
        .filter(|token| token.len() >= 2)
        .collect()
}

fn canonicalize(path: &Path) -> Result<PathBuf, FileIndexError> {
    std::fs::canonicalize(path).map_err(io_error)
}

fn io_error(error: impl ToString) -> FileIndexError {
    FileIndexError::Io(error.to_string())
}

fn is_sensitive_path(path: &Path) -> bool {
    let lower = path
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    [
        ".env",
        "id_rsa",
        "id_ed25519",
        ".pem",
        ".key",
        "secret",
        "token",
        "credential",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn read_dir_sorted(path: &Path) -> Result<Vec<fs::DirEntry>, FileIndexError> {
    let mut entries: Vec<_> = fs::read_dir(path)
        .map_err(io_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(io_error)?;
    entries.sort_by_key(|entry| entry.path());
    Ok(entries)
}

impl FileIndex {
    fn root_for_path(&self, path: &Path) -> Option<&IndexedRoot> {
        self.roots
            .iter()
            .filter(|(root_path, _)| path.starts_with(root_path))
            .max_by_key(|(root_path, _)| root_path.components().count())
            .map(|(_, root)| root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_lists_searches_and_previews_text_files() {
        let root = test_tree("file-index");
        std::fs::write(root.join("src/lib.rs"), "pub fn alpha() {}\n").expect("file should write");
        std::fs::write(root.join("README.md"), "cargo test guidance\n").expect("file should write");
        let mut index = FileIndex::new();

        let indexed_count = index.refresh_root(&root).expect("root should index");
        assert!(indexed_count >= 2);

        let listing = index.list_dir(&root, 10).expect("dir should list");
        assert_eq!(listing[0].kind, FileKind::Directory);

        let preview = index
            .preview_file(root.join("README.md"), 128, 10)
            .expect("preview should work");
        assert!(preview.is_text);
        assert!(preview.content.expect("preview content").contains("cargo"));

        let results = index
            .search(&root, "cargo readme", 10)
            .expect("search should work");
        assert!(!results.is_empty());
        assert!(results[0].entry.path.ends_with("README.md"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn preview_hides_sensitive_files() {
        let root = test_tree("file-index-sensitive");
        let secret_path = root.join(".env");
        std::fs::write(&secret_path, "TOKEN=secret\n").expect("secret file should write");
        let index = FileIndex::new();

        let preview = index
            .preview_file(&secret_path, 128, 10)
            .expect("preview should work");
        assert!(!preview.is_text);
        assert!(preview.content.is_none());
        assert_eq!(preview.sensitivity, Sensitivity::SensitiveRawContent);

        let _ = std::fs::remove_dir_all(root);
    }

    fn test_tree(prefix: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("{prefix}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).expect("tree should create");
        root
    }
}
