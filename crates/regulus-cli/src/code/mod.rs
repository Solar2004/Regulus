//! Regulus Code in the terminal (docs/04-client.md §12): `regulus code` finds or starts a
//! coding session and opens a screen for it, and `regulus code inspect` reads its
//! workspace. A client like the others: it reaches the core over the socket, knows no
//! module by its name, and what it shows is the conversation the Agent box keeps
//! (`agent.history`) and the workspace the module behind `code.inspect` serves.

pub mod composer;
pub mod inspect;
pub mod mascot;
pub mod session;
pub mod settings;
