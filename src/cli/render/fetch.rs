//! How `fetch`'s report reads on a terminal.

use crate::json::Out;
use crate::sync::fetch::*;

impl FetchReport {
    pub fn render(&self, out: Out) {
        out.say(format!(
            "fetched {} issue(s), {} changed{}",
            self.fetched,
            self.changed,
            if self.deleted > 0 {
                format!(", {} no longer in scope", self.deleted)
            } else {
                String::new()
            }
        ));
    }
}
