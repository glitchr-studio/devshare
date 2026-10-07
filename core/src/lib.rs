//! The DevShare engine: everything the CLI, the desktop app and the mobile
//! apps have in common.

pub mod control;
pub mod direct;
pub mod discover;
pub mod environment;
pub mod frame;
pub mod guest;
pub mod host;
pub mod invite;
pub mod link;
pub mod probe;
pub mod qr;

pub use devshare_protocol as protocol;
