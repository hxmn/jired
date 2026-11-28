//! How `pull`'s report reads on a terminal.

use crate::json::Out;
use crate::sync::pull::*;

impl PullReport {
    pub fn render(&self, out: Out) {
        // What came off the server is half the story of a pull, and the report
        // used to keep it only in the JSON.
        if let Some(f) = &self.fetched {
            out.say(format!(
                "fetched {} issue(s) from the server, {} changed",
                f.fetched, f.changed
            ));
        }
        for c in &self.created {
            out.say(format!("  new       {}", c.key));
        }
        for c in &self.updated {
            if c.changes.is_empty() {
                out.say(format!("  updated   {}  [{}]", c.key, c.fields.join(", ")));
                continue;
            }
            out.say(format!("  updated   {}", c.key));
            for change in &c.changes {
                out.say(format!(
                    "              {:<18} {}",
                    change.title(),
                    change.describe()
                ));
            }
        }
        for r in &self.renamed {
            out.say(format!("  renamed   {}  {} → {}", r.key, r.from, r.to));
        }
        // Shown with the report rather than as they happen: a long pull would
        // otherwise scroll them off the top before anyone read them.
        for w in &self.warnings {
            out.say(crate::style::out(
                crate::style::YELLOW,
                format!("  warning   {w}"),
            ));
        }
        for f in &self.failed {
            out.say(crate::style::out(
                crate::style::RED,
                format!("  failed    {}  {}", f.key, f.error),
            ));
        }
        for d in &self.deleted {
            out.say(format!("  gone      {}  (no longer in scope)", d.key));
        }
        let side = match self.strategy.as_str() {
            "ours" => "kept the local copy",
            _ => "took the server's copy",
        };
        for d in &self.decided {
            out.say(format!(
                "  decided   {}  [{}] — {side}",
                d.key,
                d.fields.join(", ")
            ));
        }
        if self.conflicts.is_empty() {
            let failed = match self.failed.len() {
                0 => String::new(),
                n => format!(", {n} failed"),
            };
            out.say(format!(
                "{} new, {} updated, {} unchanged{failed}",
                self.created.len(),
                self.updated.len(),
                self.unchanged
            ));
            if !self.failed.is_empty() {
                out.hint("a failed issue was left as it was; the next pull tries it again");
            }
            if self.dry_run {
                out.say("dry run — no files were written");
            }
            return;
        }

        // `merge` is the one strategy that writes despite the conflict, so the
        // summary must not claim nothing was written.
        let wrote_markers = self.strategy == "merge";
        out.say(crate::style::out(
            crate::style::RED,
            format!(
                "{} conflicting issue(s); {}",
                self.conflicts.len(),
                if wrote_markers {
                    "conflict markers were written into the files"
                } else {
                    "nothing was written"
                }
            ),
        ));
        for c in &self.conflicts {
            out.say(format!("  conflict  {}  [{}]", c.key, c.fields.join(", ")));
        }
        if wrote_markers {
            out.hint("edit the <<<<<<< ======= >>>>>>> regions, then \"jired resolve --done\"");
        } else {
            out.hint("use \"jired pull --strategy merge\" to write conflict markers");
            out.hint("or \"jired resolve --ours\" / \"--theirs\" to take one side");
        }
    }
}
