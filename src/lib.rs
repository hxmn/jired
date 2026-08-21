//! jired — offline-first Jira issues as Markdown files.
//!
//! Module map (see `docs/plan/architecture.md`):
//!
//! | module | role |
//! |---|---|
//! | [`error`] | error taxonomy and the stable exit-code table |
//! | [`json`] | versioned machine-readable output contract |
//! | [`config`] | flag → env → local → global → prompt resolution |
//! | [`workspace`] | `.jired/` layout, run lock, `.gitignore` |
//! | [`store`] | SQLite session and state databases |
//! | [`auth`] | credential storage (keychain or encrypted blob) |
//! | [`api`] | Jira REST abstraction over Cloud and Data Center |
//! | [`convert`] | rich text: ADF/wiki ⇄ `JiredAst` ⇄ Markdown |
//! | [`doc`] | issue file format: frontmatter and sections |
//! | [`sync`] | fetch/pull/push engine and three-way merge |
//! | [`project`] | filesystem projector: names, files, links |
//! | [`tui`] | full-screen interactive screens |

pub mod api;
pub mod auth;
pub mod cli;
pub mod config;
pub mod convert;
pub mod doc;
pub mod error;
pub mod json;
pub mod project;
pub mod store;
pub mod sync;
pub mod tui;
pub mod workspace;

pub use error::{ExitCode, JiredError, Result};
