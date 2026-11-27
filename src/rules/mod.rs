//! The rules of the things jired manages beside issues — sprints, releases,
//! saved filters — apart from the commands that drive them.
//!
//! A command's handler used to hold these too, and the next handler that
//! needed one called into another command to get it. Here they are a module
//! of their own: a handler parses its flags, calls these, and renders.

pub mod filter;
pub mod release;
pub mod sprint;
