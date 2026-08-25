# Changelog

## 0.1.0

- Initial Rust implementation.
- TUI and CLI provider switching for OpenCode, Claude Code, and Codex.
- Three-agent Termux diagnostics, fresh setup, install/update, verification, and stale-lock recovery.
- Self-contained aarch64 OpenCode Termux loader assets and release installer script.
- Readable Codex, Claude Code, and OpenCode session browser with normalized conversation text, filtering, grouping, and scrolling.
- Provider presets and explicit model discovery/update commands.
- Preservation of rich per-model metadata, metadata-safe model refreshes, and measured provider check latency.
- Provider duplication plus optional website and notes metadata.
- Explicit provider checks cache their redacted result, latency, and timestamp without background probing.
- Touch-first paged provider editor for connection, model, performance, capability, and profile settings.
- Automatic OpenCode live-provider merge and non-destructive OpenCode config updates.
- Touch-first single-provider setup, ordered multi-provider model routing, direct Agent install/update cards, and OpenCode permission settings.
- Added the staged CC Switch + Codex++ implementation roadmap and a raster-style terminal button system drawn directly into Ratatui buffers.
- Added a versioned unified MCP store, safe three-client projections, CLI management, and touch-first per-client enable controls.
- Added MCP live import with conflict detection, per-client import-all reports, built-in templates, and safe updates.
- Added versioned Prompt presets with three-client activation, live-file import, active deletion protection, and smart backfill of external Markdown edits.
- Added preview generation checks and made provider config plus current-state updates one rollback-aware transaction.
- Added local Skill import, deterministic content hashing, ownership-safe symlink/copy projection, verification, and touch-first three-client controls.
- Hardened writes with atomic replacement and changed Claude/Codex provider application to preserve unrelated user settings without injecting permissive Codex security policy.
- Safe dry-run, redacted diffs, backups, restore, and rollback-aware apply.
- Conservative local protocol adapter via `xu serve`.
- GitHub-ready release packaging.
- Reworked the home screen into seven first-class workspaces instead of hiding Prompts and Skills behind MCP navigation.
- Added responsive one/two-column home layout, short-screen card scrolling, grid-aware arrow navigation, and shared render/hit-test rectangles.
- Added 40/60/80-column, short-screen scrolling, mouse hit-test, and grid-navigation regression tests.
- Updated the CC Switch v3.17 parity audit and delivery order around Projects, remote Skills, whole-state export, Usage/Cost, and Universal Providers.
- Added the versioned Project store, per-target capture semantics, automatic save-on-leave switch planning, dangling-reference diagnostics, dry-run-first CLI lifecycle, and a touch-first Projects workspace.
- Added mixed file/Skill transactions, same-path Provider+MCP composition, Skill quarantine rollback, detailed redacted Project diffs, and real CLI/TUI Project switch apply with the Project current commit written last.
- Added Project rename and protected dry-run-first delete, touch delete confirmation, typed switch readiness, and a three-scope current Project summary in the responsive home header.
- Added reusable responsive stacked text-form rendering and touch hit-testing, then used it for complete in-TUI Project create and rename workflows with read-only stable IDs, validation, dry-run, and confirmation.
- Completed the Task 11 follow-up verification: format check, release build, clippy, and 467 tests pass; Zen accepts an empty provider `apiKey` and returns zero-cost responses; OpenCode text/tool, Claude text/tool, and Codex non-tool Responses text runs pass; Responses streaming now preserves accumulated `output_text.done` content; native Codex client-owned `custom`, `namespace`, and `web_search` tool types remain intentionally filtered.
