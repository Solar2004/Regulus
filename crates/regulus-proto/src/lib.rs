//! Contract types shared by the Regulus core, its clients and its modules.
//!
//! Nothing here knows about AI, agents or any specific module: the core only deals in
//! generic pieces (nodes, ports, wires, workflows, profiles and executions). JSON field
//! names match the client model in `crates/regulus-tauri/frontend/src/composition/model.ts`.

pub mod bus;
pub mod graph;
pub mod hook;
pub mod manifest;
pub mod rpc;

pub use bus::*;
pub use graph::*;
pub use hook::*;
pub use manifest::*;
pub use rpc::*;
