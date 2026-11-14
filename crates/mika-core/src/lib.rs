//! Core of the assistant. No Tauri or Windows dependencies except `keyring`.

pub mod agent;
pub mod backup;
pub mod bgjobs;
pub mod commands;
pub mod config;
pub mod confirm;
pub mod costs;
pub mod daylog;
pub mod emotion;
pub mod events;
pub mod gate;
pub mod hotkeys;
pub mod i18n;
pub mod llm;
pub mod marks;
pub mod memory;
pub mod paths;
pub mod policy;
pub mod schedule;
pub mod secrets;
pub mod session;
pub mod sound;
pub mod sound_capture;
pub mod speech_cache;
pub mod state;
pub mod storage;
pub mod task;
pub mod tools;
pub mod triggers;

pub use emotion::Emotion;
pub use events::{Event, EventBus};
pub use state::{AssistantState, StateHandle};
