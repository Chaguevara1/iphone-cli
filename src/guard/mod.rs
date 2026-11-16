//! The guard: before iph sends input that may pay, send, delete, grant a
//! permission or reveal a secret, it asks the person at the computer (mode
//! `human`, the default), refuses (`deny`), or lets it through (`off`).
//! `[guard] only_apps` and `block_apps` apply in every mode.
//!
//! It defends against an agent whose only channel to the phone is iph (MCP
//! tools or the CLI). It does not stop an agent with a general shell running
//! as the same user: that one can edit the config or answer the dialog.

pub mod confirm;
pub mod rules;

pub use confirm::{Answer, Ask, Confirmer, Prompt, Scripted, build_prompt, dialog_main};
pub use rules::{Action, Concern, Rules, normalize};
