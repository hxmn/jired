//! How the engine's reports read on a terminal.
//!
//! The reports are the engine's; how a person sees them is the command line's.
//! The `render` methods lived beside the structs in `sync`, so the engine
//! carried the colours, the column widths and the hints for `jired` commands
//! with it. They are inherent methods still, written here, so every caller
//! that says `report.render(out)` reads the same.

mod diff;
mod fetch;
mod pull;
mod push;
mod reset;
mod status;
