// SPDX-License-Identifier: Apache-2.0

//! The GUI's own folder (`--work`): the one place the GUI keeps state.
//!
//! Only this module and the preview cache (`thumb.rs`) call the write API;
//! `tests/gui_lessons.rs` fails if another GUI file calls `dest::`.

use std::path::Path;

use albumen_ingest::{dest, IngestError, SourceRoot};

/// Refuse `path` if it is the library, inside it, or hard-linked into it,
/// then make sure it exists.
pub fn ensure(library: &Path, path: &Path) -> Result<(), IngestError> {
    SourceRoot::open(library)?.refuse_destinations(&[path])?;
    dest::create_dir_all(path)
}
