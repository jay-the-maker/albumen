// SPDX-License-Identifier: Apache-2.0

//! Receivers, importers, and MCP tools do not unlink a stored original.
//! Removal is a tombstone or a hide. Rollback of a file this call has not
//! finished verifying stays in the ingest store, not in these surfaces.

use std::fs;
use std::path::PathBuf;

#[test]
fn receivers_importers_and_mcp_have_no_unlink_of_a_stored_original() {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = vec![
        "receive/src/lib.rs",
        "receive/src/store.rs",
        "receive/src/webdav.rs",
        "receive/src/sftp.rs",
        "receive/src/ftp.rs",
        "mcp/src/lib.rs",
        "mcp/src/main.rs",
        "mcp/src/tools.rs",
        "cli/src/main.rs",
        "cli/src/receive.rs",
        "ingest/src/takeout.rs",
    ];
    for extra in ["ingest/src/icloud.rs", "ingest/src/photos_library.rs"] {
        if workspace.join(extra).is_file() {
            files.push(extra);
        }
    }
    for relative in files {
        let path = workspace.join(relative);
        let text = fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {relative}: {err}"));
        for (index, line) in text.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("//") || trimmed.starts_with("//!") {
                continue;
            }
            let unlinks = trimmed.contains("remove_file")
                || trimmed.contains("remove_dir")
                || trimmed.contains("unlink(");
            if !unlinks {
                continue;
            }
            assert!(
                trimmed.contains("remove_file(&tmp)"),
                "{}:{} unlinks a stored file:\n{line}",
                relative,
                index + 1
            );
        }
    }
}
