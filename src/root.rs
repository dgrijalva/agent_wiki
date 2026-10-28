//! Where a wiki's documents come from: a folder on disk, or files baked into
//! the build.

use std::io::ErrorKind;
use std::path::Path;

use crate::index::{FsKind, fs_kind};

pub(crate) enum Root {
    /// A folder on disk, canonical.
    Disk(std::path::PathBuf),
    /// Files baked into the build: each `/`-separated path (with `.md`) and
    /// its text.
    Embedded(Vec<(&'static str, &'static str)>),
}

impl Root {
    /// What is at `rel` (`""` is the root itself). On disk, without
    /// following symlinks; embedded, a folder is any path some file is
    /// under.
    pub(crate) async fn kind(&self, rel: &str) -> std::io::Result<FsKind> {
        match self {
            Root::Disk(root) => fs_kind(root, rel).await,
            Root::Embedded(files) => {
                let rel = rel.trim_matches('/');
                if rel.is_empty() {
                    return Ok(FsKind::Dir);
                }
                let under = format!("{rel}/");
                Ok(if files.iter().any(|(path, _)| *path == rel) {
                    FsKind::File
                } else if files.iter().any(|(path, _)| path.starts_with(&under)) {
                    FsKind::Dir
                } else {
                    FsKind::Missing
                })
            }
        }
    }

    /// The text of the file at `rel`.
    pub(crate) async fn read(&self, rel: &str) -> std::io::Result<String> {
        match self {
            Root::Disk(root) => tokio::fs::read_to_string(root.join(rel)).await,
            Root::Embedded(files) => files
                .iter()
                .find(|(path, _)| *path == rel)
                .map(|(_, text)| text.to_string())
                .ok_or_else(|| ErrorKind::NotFound.into()),
        }
    }

    /// The folder on disk, if the documents are there.
    pub(crate) fn disk(&self) -> Option<&Path> {
        match self {
            Root::Disk(root) => Some(root),
            Root::Embedded(_) => None,
        }
    }
}
