//! Events and watching (decision 0031): scenarios that run when something
//! happens on the computer, and "wait and tell".
//!
//! One watcher looks at the system every few seconds, and only at what the
//! enabled scenarios and the running watches need. The model takes no part in
//! watching. A scenario that fires becomes an ordinary task: each of its
//! actions goes through the risk policy like any other.

mod sensors;
mod service;
mod trigger_tools;
mod wait_tools;
mod waits;

pub(crate) use sensors::OwnInput;
pub use sensors::{AudioDevice, Proc, Sensors, SessionState};
pub use service::{run, Handler, Service, ServiceDeps, TICK};
pub use trigger_tools::{TriggerCreateTool, TriggerDeleteTool, TriggerListTool};
pub use wait_tools::{WaitEnv, WaitForTool, WaitListTool, WaitStopTool};
pub use waits::{
    Condition, WaitView, Waits, DEFAULT_TIMEOUT_MIN, MAX_PHRASE, MAX_SAY, MAX_TIMEOUT_MIN,
    MAX_WAITS, QUIET_S,
};
