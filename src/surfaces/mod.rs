//! Where devices are shown and controlled: Apple Home, the control socket
//! (the menu bar and scripts), and the CLI. Readings over time are asked for
//! the same way; the files live in `series`.

pub mod cli;
pub mod control;
pub mod homekit;
