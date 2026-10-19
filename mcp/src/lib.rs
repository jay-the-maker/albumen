// SPDX-License-Identifier: Apache-2.0

//! MCP library surface. The `albumen-mcp` binary is a stdio loop over this.
//!
//! Search and browse only read. Export and upload go through `copy_verify`
//! and are off unless the process asks for them. Even then they write only
//! when the process was started with `--apply` or `--execute`. There is no
//! delete tool.

pub mod io;
pub mod metadata;
pub mod mount;
pub mod page;
pub mod thumbnail;
pub mod tools;

pub use tools::Library;
