//! The rules of the things jired manages — sprints, releases, saved filters,
//! a parent's sub-tasks — apart from the commands and the store that drive them.
//!
//! A command's handler used to hold these too, and the next handler that
//! needed one called into another command to get it. Here they are a module
//! of their own: a handler parses its flags, calls these, and renders.

pub mod filter;
pub mod release;
pub mod sprint;
pub mod subtasks;
