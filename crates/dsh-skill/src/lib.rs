//! Dual-format skill discovery: OpenAI SKILL.md + DeepSeek/dsh skills.

mod catalog;
mod meta;
mod parse;
pub mod router;
mod tools;

pub use catalog::{SkillCatalog, SkillRecord, SkillResources, SkillSource, SkillSummary};
pub use meta::{auto_tag_and_examples, SkillMeta};
pub use router::{prompt_topk_section, rank_skills, RankedSkill};
pub use tools::register_skill_tools;
