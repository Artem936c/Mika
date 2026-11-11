//! Core of the assistant. No Tauri or Windows dependencies except `keyring`.

pub mod agent;
pub mod config;
pub mod confirm;
pub mod events;
pub mod gate;
pub mod llm;
pub mod paths;
pub mod policy;
pub mod secrets;
pub mod session;
pub mod state;
pub mod storage;
pub mod tools;

pub use events::{Event, EventBus};
pub use state::{AssistantState, StateHandle};
