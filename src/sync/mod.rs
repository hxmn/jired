//! Sync engine: fetch, merge, diff, push [M1.8, M2].

pub mod attachments;
pub mod diff;
pub mod fetch;
pub mod group;
pub mod merge;
pub mod progress;
pub mod pull;
pub mod push;
pub mod render;
pub mod reset;
pub mod screen;
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
    /// `ignored_custom_fields` as configured — ids or names, resolved against
    /// the instance's field metadata by [`Engine::field_map`].
    pub ignored_custom_fields: Vec<String>,
    /// `checklist.field`, resolved the same way and for the same reason.
    pub checklist_field: Option<String>,
}

impl Engine {
    pub fn open(ctx: &mut Context) -> Result<Self> {
        let session = SessionStore::open(&ctx.workspace)?;
        let account = session.require_account()?;
        let engine = Self {
            state: StateStore::open(&ctx.workspace)?,
            projector: Projector::new(ctx.workspace.root()),
            account,
            ignored_custom_fields: ctx.resolver.get_list("ignored_custom_fields")?,
            checklist_field: ctx.resolver.get_value("checklist.field")?,
        };
        engine.adopt_new_issue_files()?;
        Ok(engine)
    }

    /// Give every `NEW-` file on disk a row. A locally created issue exists
    /// only as its file and its row, so after `state.db` was rebuilt, as its
    /// own hints suggest, `status` and `push` iterate rows and never saw it,
    /// and `push NEW-1` said it was not in the workspace.
    fn adopt_new_issue_files(&self) -> Result<()> {
        let Ok(files) = self.projector.scan() else {
            return Ok(());
        };
        for (key, file) in files.range("NEW-".to_string()..) {
            if !key.starts_with("NEW-") {
                break;
            }
            self.state.adopt_new_issue(key, &file.relative)?;
        }
        Ok(())
    }

    pub fn markup(&self) -> Markup {
        self.account.deployment.markup()
    }

    /// Field definitions cached by the last fetch, plus this workspace's
    /// ignore list resolved against them.
    ///
    /// Every path that re-parses a stored snapshot must use this, or
    /// instance-specific custom fields decay into unmapped `custom:` entries
    /// and ignored ones come back.
    pub fn field_map(&self) -> Result<crate::api::normalize::FieldMap> {
        let mut map = self.state.field_map()?;
        map.ignore(&self.ignored_custom_fields);
        if let Some(field) = &self.checklist_field {
            map.set_checklist_text(field);
        }
        Ok(map)
    }

    pub fn base_url(&self) -> &str {
        &self.account.base_url
    }

    pub fn client(&self, ctx: &mut Context) -> Result<Box<dyn JiraApi>> {
        ctx.client_for(&self.account)
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
