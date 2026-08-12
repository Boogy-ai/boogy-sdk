# Boogy service project

This is a Boogy service (Rust → wasm32-wasip2). Expert skills for
building Boogy services are available as agent skills.

**If the Boogy skills are not yet in place:**
- **Claude Code (preferred):** `claude plugin marketplace add Boogy-ai/boogy-superpowers` then
  `claude plugin install boogy-superpowers`. Tell the human to run **`/reload-plugins`** so the
  plugin (skills + MCP + onramp gate) activates mid-session.
- **Vendor / other agents:** `boogy skills install` (or
  `npx degit Boogy-ai/boogy-superpowers/skills .claude/skills` — flat, no wrapper suffix).
  Skills load automatically; if `.claude/skills/` was just created, tell the human to **restart Claude Code**.

Then start with the `using-boogy` skill.**
