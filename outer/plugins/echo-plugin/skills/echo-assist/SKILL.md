---
name: echo-assist
description: Guide the agent to use the outer echo plugin for simple round-trip checks and sandbox host bridges.
whenToUse: When verifying plugin tooling, Rhai sandbox, or demonstrating outer-layer plugin calls.
tags:
  - plugin
  - echo
  - utility
---

# Echo Assist

Use this skill to validate outer-layer plugins without touching core crates.

## Steps

1. Call `plugin_search` with query `echo`.
2. Invoke `plugin.echo.echo` with `{ "input": "hello" }`.
3. Expect a response starting with `echo:`.

## Examples

- Verify the echo plugin after install
- Smoke-test Rhai host bridges from a plugin tool
