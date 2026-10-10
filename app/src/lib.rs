//! The desktop app, less its window: what the window displays and what it
//! can ask for. Kept apart from the window so it can be tested without one.

pub mod commands;
pub mod joined;
pub mod native;
pub mod owners;
pub mod projects;
pub mod session;
pub mod system;
