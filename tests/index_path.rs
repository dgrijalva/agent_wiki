//! `FilePath` names that SQLite would otherwise interpret. This file holds a
//! single test because it changes the process's working directory.

use std::fs;

use agent_wiki::{IndexStorage, Wiki};

#[tokio::test]
async fn special_sqlite_names_are_ordinary_files() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("a.md"), "# A\n").unwrap();
    let cwd = tempfile::tempdir().unwrap();
    std::env::set_current_dir(cwd.path()).unwrap();
    for name in [":memory:", "file:x?nolock=1", "file:y.db?mode=memory"] {
        let storage = IndexStorage::FilePath(name.into());
        let first = Wiki::builder(root.path())
            .index_storage(storage.clone())
            .build()
            .await
            .unwrap();
        first.indexed().await;
        let file = cwd.path().join(name);
        assert!(file.is_file(), "{name}: no file at {}", file.display());
        for second in [storage, IndexStorage::FilePath(file.clone())] {
            let Err(e) = Wiki::builder(root.path())
                .index_storage(second.clone())
                .build()
                .await
            else {
                panic!("{name}: a second opener succeeded with {second:?}");
            };
            assert!(format!("{e:#}").contains("lock"), "{name}: {e:#}");
        }
        drop(first);
    }
    // Nothing but the index files (and their `-wal` files): no folders.
    for entry in fs::read_dir(cwd.path()).unwrap() {
        let entry = entry.unwrap();
        assert!(entry.file_type().unwrap().is_file(), "{entry:?}");
    }
}
