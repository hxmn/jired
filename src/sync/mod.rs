//! Sync engine: fetch, merge, diff, push [M1.8, M2].

pub mod diff;
pub mod fetch;
pub mod merge;
pub mod pull;
pub mod push;
pub mod render;
pub mod status;

use crate::api::{JiraApi, Markup};
use crate::cli::context::Context;
use crate::error::Result;
use crate::project::Projector;
use crate::store::session::{Account, SessionStore};
use crate::store::state::StateStore;

/// The handles every sync command needs, opened once.
pub struct Engine {
    pub state: StateStore,
    pub projector: Projector,
    pub account: Account,
}

impl Engine {
    pub fn open(ctx: &Context) -> Result<Self> {
        let session = SessionStore::open(&ctx.workspace)?;
        let account = session.require_account()?;
        Ok(Self {
            state: StateStore::open(&ctx.workspace)?,
            projector: Projector::new(ctx.workspace.root()),
            account,
        })
    }

    pub fn markup(&self) -> Markup {
        self.account.deployment.markup()
    }

    /// Field definitions cached by the last fetch.
    ///
    /// Every path that re-parses a stored snapshot must use this, or
    /// instance-specific custom fields decay into unmapped `custom:` entries.
    pub fn field_map(&self) -> Result<crate::api::normalize::FieldMap> {
        self.state.field_map()
    }

    pub fn base_url(&self) -> &str {
        &self.account.base_url
    }

    pub fn client(&self, ctx: &mut Context) -> Result<Box<dyn JiraApi>> {
        crate::api::build_client(ctx, &self.account)
    }
}

/// Escape a value for use inside a double-quoted JQL literal.
pub fn jql_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jql_literals_are_escaped() {
        assert_eq!(jql_quote("Sprint 42"), "\"Sprint 42\"");
        assert_eq!(jql_quote(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(jql_quote(r"back\slash"), r#""back\\slash""#);
    }
}
