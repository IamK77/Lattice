//! Official minimal components. These exist to prove the wiring: the heartbeat
//! milestone runs a full user → model → tool → output loop through them,
//! deterministically, as the CI regression for the whole kernel.

pub mod anthropic_model;
pub mod browser_driver;
pub mod browser_tools;
pub(crate) mod call_purpose;
pub mod code_tools;
pub mod context_gate;
pub mod desktop_cua;
pub mod desktop_driver;
pub mod desktop_mcp;
pub mod desktop_scope;
pub mod desktop_tools;
pub mod effects_policy;
pub mod environment;
pub mod expert_definitions;
pub mod expert_ui;
pub mod fs_tools;
pub mod fs_watch;
pub mod interface_permissions;
pub mod media_document;
pub mod minimal_loop;
pub mod model_common;
mod model_http;
pub mod net_tools;
pub mod openai_model;
pub mod operation_policy;
pub mod project_rules;
pub mod responses_media;
pub mod responses_model;
pub mod responses_wire;
pub mod scripted_model;
pub mod search_tools;
pub mod shell_tools;
pub mod silent_ui;
pub mod skill_library;
pub mod subagent;
mod subscription_history;
pub mod timer_tools;
mod tool_artifacts;
pub mod tool_catalog;
pub mod trust_policy;
pub mod web_search;
mod web_text;
pub mod workshop_sink;
