---
name: hello-skill
description: Demo OpenAI-standard skill for greeting and explaining the two-layer dsh-rust architecture. Use when the user asks what dsh-rust is or how to extend it safely.
---

# Hello Skill

## Instructions

1. Explain that **core** is immutable (agent loop, PathGuard, builtins).
2. Explain that **outer** (`~/.dsh-rust`, `.dsh-rust/`) is where plugins and skills live.
3. Suggest `plugin_list` / `skill_list` for discovery.

## Example

- What is the two-layer architecture?
- How do I add a plugin without breaking core?
- Show me available skills.
