//! Outer-layer plugin registry with Rhai sandbox and hot-reload.

mod loader;
mod meta;
mod registry;
pub mod router;
mod runtime;
mod tools;

pub use loader::{
    install_plugin_from_path, load_plugin_dir, plugin_skill_paths, PluginManifest, PluginToolDecl,
};
pub use meta::{auto_tag_plugin, PluginMeta};
pub use registry::{PluginLoadEvent, PluginRegistry, RoutingSummary};
pub use router::{prompt_topk_section, rank_plugins, RankedPlugin};
pub use tools::{register_plugin_tools, register_plugin_tools_with_weights, LearnWeightProvider};
