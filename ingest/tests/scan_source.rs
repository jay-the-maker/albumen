// SPDX-License-Identifier: Apache-2.0

//! The scan path must not write a source timestamp.

use std::fs;
use std::path::PathBuf;

#[test]
fn scan_code_does_not_set_timestamps() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let paths = [root.join("src/scan.rs")];
    let needles = ["set_times", "set_modified", "utimes", "filetime"];
    for path in paths {
        let text = fs::read_to_string(&path).expect("read scan source");
        assert!(!text.is_empty(), "{}", path.display());
        for needle in needles {
            assert!(
                !text.contains(needle),
                "{} contains {needle}",
                path.display()
            );
        }
    }
}
