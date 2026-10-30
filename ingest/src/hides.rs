// SPDX-License-Identifier: Apache-2.0

//! Hide and delete state that a listing must honour (M7, A8).
//!
//! STEP A STUB: returns nothing hidden, so the M7 tests compile and fail.
//! The fix derives the set from the sidecars and the family log.

use std::collections::BTreeSet;
use std::path::Path;

/// What [`hidden_paths`] found.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Hidden {
    /// Library-relative paths a listing must not show.
    pub paths: BTreeSet<String>,
    /// Problems reading hide state, in plain words.
    pub errors: Vec<String>,
}

/// The paths among `paths` (library-relative, as the index lists them) that
/// are hidden or deleted for `viewer` (a family member id, or None).
pub fn hidden_paths(_library: &Path, _paths: &[String], _viewer: Option<&str>) -> Hidden {
    Hidden::default()
}
