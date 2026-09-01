//! Native integration adapters (hooks/MCP/instruction writers).
//!
//! Harness identity, aliases, detection markers, skills, projection policy,
//! and delegation are authoritative in
//! `contracts/stateroot_harness_registry.v1.json`. This table contains only
//! executable native integration details that cannot be represented as generic
//! data (format-specific merge/render behavior).

use std::path::{Path, PathBuf};

/// MCP config file shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpShape {
    /// Top-level `mcpServers` object (claude, cursor, kimi*, opencode).
    McpServersJson,
    /// Top-level `servers` object (vscode-copilot — NOTE: not `mcpServers`).
    ServersJson,
    /// YAML `mcp_servers` mapping (hermes `~/.hermes/config.yaml`; setup
    /// writes MCP server defs there as a dict — `hermes_cli/config.py`).
    YamlMcpServers,
}

/// One MCP registration target.
#[derive(Debug, Clone, Copy)]
pub struct McpTarget {
    /// Home-relative config file.
    pub path: &'static str,
    /// File shape.
    pub shape: McpShape,
}

/// Native lifecycle-hook config format (ai-memory shapes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookFormat {
    /// `"E": [{"matcher": "", "hooks": [{"type": "command", "command": "…"}]}]`
    /// (claude-code, codex, gemini-cli, grok, devin).
    NestedJson,
    /// `"e": [{"type": "command", "command": "…", "matcher": ""}]` with a
    /// top-level `version: 1` sibling (cursor).
    FlatJson,
    /// `[[hooks]]` entries `{event, command}` inside config.toml (kimi-code, kimi).
    TomlHooks,
    /// `{"enabled": true, "hooks": [{"id", "name", "event", "command", "args", "enabled"}]}`
    /// — exec form with JSON payload on stdin (zero).
    ZeroExecJson,
    /// Named-groups `{"stateroot": {<events>}}`: tool events nested, lifecycle
    /// events flat handler list (antigravity).
    NamedGroupsJson,
    /// Generated native plugin package (openclaw).
    NativePlugin,
}

/// One hook target: home-relative file (or directory for plugins).
#[derive(Debug, Clone, Copy)]
pub struct HookTarget {
    /// Home-relative path of the hook config (or plugin directory).
    pub path: &'static str,
    /// Config format.
    pub format: HookFormat,
}

/// Whether this harness can inject identity into the model automatically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryTier {
    /// Session-start and/or first-prompt injects identity into model context.
    Automatic,
    /// No verified injection channel — instruction file and/or MCP pull only.
    Degraded,
}

/// Per-harness policy for getting identity onto the first usable prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DigestDeliveryPolicy {
    /// Canonical event that is supposed to inject first (`session_start` or
    /// `user_prompt_submit`). Empty when the harness cannot inject.
    pub primary_event: &'static str,
    /// Print digest on `session_start` (side effects still always run).
    pub session_start_prints: bool,
    /// Count a `session_start` print as delivered. False when that stdout is
    /// discarded by the harness (Cursor, Kimi Code, OpenClaw, McpPull).
    pub session_start_marks: bool,
    /// Print digest on `user_prompt_submit` when the session is still unmarked
    /// (or this event *is* the primary injection channel).
    pub prompt_submit_injects: bool,
    /// Honest capability tier.
    pub tier: DeliveryTier,
    /// Short note for doctor/install.
    pub note: &'static str,
}

impl DigestDeliveryPolicy {
    /// Policy for a canonical adapter id. Unknown ids are degraded.
    pub fn for_id(id: &str) -> Self {
        match id {
            "claude-code" | "codex" | "kimi" | "devin" => Self {
                primary_event: "session_start",
                session_start_prints: true,
                session_start_marks: true,
                prompt_submit_injects: true,
                tier: DeliveryTier::Automatic,
                note: "session-start inject; first prompt retries if that missed",
            },
            "cursor" => Self {
                primary_event: "session_start",
                session_start_prints: true,
                session_start_marks: true,
                // Cursor's beforeSubmitPrompt is continue-only (no
                // additional_context) — prompt submits are capture-only.
                // postToolUse provides the post-compaction/fallback channel.
                prompt_submit_injects: false,
                tier: DeliveryTier::Automatic,
                note: "session-start inject; postToolUse restores identity after compaction",
            },
            "gemini-cli" | "antigravity" => Self {
                primary_event: "session_start",
                session_start_prints: true,
                session_start_marks: true,
                prompt_submit_injects: false,
                tier: DeliveryTier::Automatic,
                note: "session-start inject; no prompt-submit fallback on this harness",
            },
            "kimi-code" => Self {
                primary_event: "user_prompt_submit",
                session_start_prints: false,
                session_start_marks: false,
                prompt_submit_injects: true,
                tier: DeliveryTier::Automatic,
                note: "SessionStart stdout is discarded; identity rides UserPromptSubmit",
            },
            "openclaw" => Self {
                primary_event: "user_prompt_submit",
                session_start_prints: true,
                session_start_marks: false,
                prompt_submit_injects: true,
                tier: DeliveryTier::Automatic,
                note: "before_prompt_build pulls digest; session_start stdout is discarded",
            },
            "grok" => Self {
                primary_event: "session_start",
                session_start_prints: true,
                session_start_marks: false,
                prompt_submit_injects: true,
                tier: DeliveryTier::Automatic,
                note: "McpPull session-start is unreliable; first prompt retries",
            },
            "opencode" | "omp" => Self {
                primary_event: "user_prompt_submit",
                session_start_prints: true,
                session_start_marks: false,
                prompt_submit_injects: true,
                tier: DeliveryTier::Automatic,
                note: "generated plugin consumes hook stdout on the first prompt",
            },
            "pi" => Self {
                primary_event: "user_prompt_submit",
                session_start_prints: true,
                session_start_marks: false,
                prompt_submit_injects: true,
                tier: DeliveryTier::Automatic,
                note: "before_agent_start injects a session message on the first prompt",
            },
            "zero" => Self {
                primary_event: "session_start",
                session_start_prints: true,
                session_start_marks: false,
                prompt_submit_injects: false,
                tier: DeliveryTier::Degraded,
                note: "no verified prompt injection; run `stateroot resume` if identity is missing",
            },
            "hermes" => Self {
                primary_event: "",
                session_start_prints: false,
                session_start_marks: false,
                prompt_submit_injects: false,
                tier: DeliveryTier::Degraded,
                note: "no hooks in v1 — resume via MCP / `stateroot resume --harness hermes`",
            },
            "vscode-copilot" | "crush" => Self {
                primary_event: "",
                session_start_prints: false,
                session_start_marks: false,
                prompt_submit_injects: false,
                tier: DeliveryTier::Degraded,
                note: "instruction-file protocol only; run `stateroot resume` at session start",
            },
            _ => Self {
                primary_event: "",
                session_start_prints: false,
                session_start_marks: false,
                prompt_submit_injects: false,
                tier: DeliveryTier::Degraded,
                note: "no verified injection channel",
            },
        }
    }
}

/// How resume content reaches the model for this harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Injection {
    /// `hookSpecificOutput.additionalContext` JSON envelope on stdout
    /// (claude-code, codex, devin).
    StdoutJson,
    /// Cursor native hooks: `{ "additional_context": "…" }` on stdout.
    CursorJson,
    /// Plain text on stdout (gemini-cli, antigravity, kimi).
    StdoutText,
    /// Stdout is discarded on SessionStart; resume fires on UserPromptSubmit
    /// (kimi-code).
    UserPromptSubmit,
    /// Harness/plugin pulls digest: hook still prints stdout for capture
    /// (openclaw, grok, zero, hermes).
    McpPull,
    /// No hook stdout channel — instruction file carries the digest protocol
    /// (vscode-copilot, crush).
    None,
}

/// Event-support bitflags.
pub mod event_support {
    /// Session start.
    pub const SESSION_START: u32 = 1 << 0;
    /// User prompt submit.
    pub const USER_PROMPT_SUBMIT: u32 = 1 << 1;
    /// Post tool use.
    pub const POST_TOOL_USE: u32 = 1 << 2;
    /// Tool failure.
    pub const TOOL_FAILURE: u32 = 1 << 3;
    /// Pre-compact.
    pub const PRE_COMPACT: u32 = 1 << 4;
    /// Stop.
    pub const STOP: u32 = 1 << 5;
    /// Session end.
    pub const SESSION_END: u32 = 1 << 6;
    /// Subagent boundaries.
    pub const SUBAGENT: u32 = 1 << 7;
    /// All of the above.
    pub const ALL: u32 = (1 << 8) - 1;
}

/// Registry tier (install completeness).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Full: block + MCP + native hooks.
    A,
    /// Registry row now; generated TS plugin installer in P2.
    B,
    /// MCP-only or managed-only placeholder.
    C,
}

/// One harness-specific native integration adapter.
#[derive(Debug, Clone, Copy)]
pub struct HarnessQuirk {
    /// Canonical id (`claude-code`, `kimi-code`, …).
    pub id: &'static str,
    /// Display name.
    pub display: &'static str,
    /// Tier.
    pub tier: Tier,
    /// Home-relative detect markers (any hit = present).
    pub detect: &'static [&'static str],
    /// Binary/command names probed on PATH for detection (any hit = binary
    /// present). Cross-checked against ai-memory's install surface
    /// (`ai-memory-cli/src/cli.rs` — antigravity's `agy` alias included).
    pub detect_cmds: &'static [&'static str],
    /// Global instruction file for the one-agent block (home-relative).
    pub instruction_file: Option<&'static str>,
    /// MCP registration target.
    pub mcp: Option<McpTarget>,
    /// Native hook target.
    pub hooks: Option<HookTarget>,
    /// Resume injection channel.
    pub injection: Injection,
    /// Harness injects hook stdout into the post-compaction context (Claude
    /// Code pattern): `pre_compact`/`post_compaction` checkpoints ALSO print
    /// the bounded hook digest so state is re-injected at compaction time.
    pub compact_injection: bool,
    /// Event support bitflags.
    pub events: u32,
    /// Legacy CLI id for the pre-registry harnesses (compat projection).
    pub legacy_id: Option<&'static str>,
    /// Harness-native event vocabulary: `(harness_event, canonical_event)`.
    pub event_map: &'static [(&'static str, &'static str)],
}

impl HarnessQuirk {
    /// Identity/resume delivery policy for this adapter.
    pub fn delivery(&self) -> DigestDeliveryPolicy {
        DigestDeliveryPolicy::for_id(self.id)
    }
}

use event_support as es;

/// Claude-Code shared event vocabulary (9 events; codex/gemini/devin/grok vary).
const CLAUDE_EVENTS: &[(&str, &str)] = &[
    ("SessionStart", "session_start"),
    ("UserPromptSubmit", "user_prompt_submit"),
    ("PreToolUse", "pre_tool_use"),
    ("PostToolUse", "post_tool_use"),
    ("PreCompact", "pre_compact"),
    ("Stop", "stop"),
    ("SessionEnd", "session_end"),
    ("SubagentStart", "subagent_start"),
    ("SubagentStop", "subagent_stop"),
];

const CODEX_EVENTS: &[(&str, &str)] = &[
    ("SessionStart", "session_start"),
    ("UserPromptSubmit", "user_prompt_submit"),
    ("PreToolUse", "pre_tool_use"),
    ("PostToolUse", "post_tool_use"),
    ("PreCompact", "pre_compact"),
    ("Stop", "stop"),
];

const CURSOR_EVENTS: &[(&str, &str)] = &[
    ("sessionStart", "session_start"),
    ("sessionEnd", "session_end"),
    ("beforeSubmitPrompt", "user_prompt_submit"),
    ("preToolUse", "pre_tool_use"),
    ("postToolUse", "post_tool_use"),
    ("postToolUseFailure", "tool_failure"),
    ("preCompact", "pre_compact"),
    ("stop", "stop"),
];

const GEMINI_EVENTS: &[(&str, &str)] = &[
    ("SessionStart", "session_start"),
    ("SessionEnd", "session_end"),
    ("BeforeTool", "pre_tool_use"),
    ("AfterTool", "post_tool_use"),
    ("PreCompress", "pre_compact"),
];

const KIMI_CODE_EVENTS: &[(&str, &str)] = &[
    ("SessionStart", "session_start"),
    ("UserPromptSubmit", "user_prompt_submit"),
    ("PreToolUse", "pre_tool_use"),
    ("PostToolUse", "post_tool_use"),
    ("PostToolUseFailure", "tool_failure"),
    ("PreCompact", "pre_compact"),
    ("PostCompact", "post_compaction"),
    ("Stop", "stop"),
    ("SessionEnd", "session_end"),
];

const DEVIN_EVENTS: &[(&str, &str)] = &[
    ("SessionStart", "session_start"),
    ("UserPromptSubmit", "user_prompt_submit"),
    ("PreToolUse", "pre_tool_use"),
    ("PostToolUse", "post_tool_use"),
    ("PostCompaction", "post_compaction"),
    ("Stop", "stop"),
    ("SessionEnd", "session_end"),
];

const GROK_EVENTS: &[(&str, &str)] = CLAUDE_EVENTS;

const ZERO_EVENTS: &[(&str, &str)] = &[
    ("sessionStart", "session_start"),
    ("sessionEnd", "session_end"),
    ("beforeTool", "pre_tool_use"),
    ("afterTool", "post_tool_use"),
    ("specialistStart", "subagent_start"),
    ("specialistStop", "subagent_stop"),
];

const ANTIGRAVITY_EVENTS: &[(&str, &str)] = &[
    ("PreInvocation", "session_start"),
    ("PreToolUse", "pre_tool_use"),
    ("PostToolUse", "post_tool_use"),
    ("Stop", "stop"),
];

const OPENCLAW_EVENTS: &[(&str, &str)] = &[
    // Real OpenClaw typed hook names (api.on) → stateroot canonical events.
    // Invented camelCase names (sessionStart, postToolUse, …) are NOT valid.
    ("session_start", "session_start"),
    ("session_end", "session_end"),
    ("before_prompt_build", "user_prompt_submit"),
    ("after_tool_call", "post_tool_use"),
    ("before_compaction", "pre_compact"),
    ("agent_end", "stop"),
];

const TIER_B_EVENTS: &[(&str, &str)] = &[
    ("session_start", "session_start"),
    ("user_prompt_submit", "user_prompt_submit"),
    ("post_tool_use", "post_tool_use"),
    ("pre_compact", "pre_compact"),
    ("stop", "stop"),
    ("session_end", "session_end"),
];

/// Pi 0.84 extension events (`packages/coding-agent` `ExtensionAPI.on`).
/// `session_start` is void for model context; identity rides `before_agent_start`.
const PI_EVENTS: &[(&str, &str)] = &[
    ("session_start", "session_start"),
    ("before_agent_start", "user_prompt_submit"),
    ("user_prompt_submit", "user_prompt_submit"),
    ("tool_call", "pre_tool_use"),
    ("pre_tool_use", "pre_tool_use"),
    ("tool_result", "post_tool_use"),
    ("post_tool_use", "post_tool_use"),
    ("session_before_compact", "pre_compact"),
    ("pre_compact", "pre_compact"),
    ("session_compact", "post_compaction"),
    ("agent_end", "stop"),
    ("stop", "stop"),
    ("session_shutdown", "session_end"),
    ("session_end", "session_end"),
];

/// Native hook/MCP adapters. This is deliberately not the harness registry;
/// see `skill_federation::load_registry()` for the shared contract.
pub const ADAPTERS: &[HarnessQuirk] = &[
    HarnessQuirk {
        id: "claude-code",
        display: "Claude Code",
        tier: Tier::A,
        detect: &[".claude", ".claude.json"],
        detect_cmds: &["claude"],
        instruction_file: Some(".claude/CLAUDE.md"),
        mcp: Some(McpTarget {
            path: ".claude.json",
            shape: McpShape::McpServersJson,
        }),
        hooks: Some(HookTarget {
            path: ".claude/settings.json",
            format: HookFormat::NestedJson,
        }),
        injection: Injection::StdoutJson,
        compact_injection: true,
        events: es::ALL,
        legacy_id: Some("claude"),
        event_map: CLAUDE_EVENTS,
    },
    HarnessQuirk {
        id: "codex",
        display: "Codex",
        tier: Tier::A,
        detect: &[".codex"],
        detect_cmds: &["codex"],
        instruction_file: Some(".codex/AGENTS.md"),
        mcp: None,
        hooks: Some(HookTarget {
            path: ".codex/hooks.json",
            format: HookFormat::NestedJson,
        }),
        injection: Injection::StdoutJson,
        compact_injection: false,
        events: es::ALL & !es::SESSION_END,
        legacy_id: Some("codex"),
        event_map: CODEX_EVENTS,
    },
    HarnessQuirk {
        id: "cursor",
        display: "Cursor",
        tier: Tier::A,
        detect: &[".cursor"],
        detect_cmds: &["cursor"],
        instruction_file: Some(".cursor/AGENTS.md"),
        mcp: Some(McpTarget {
            path: ".cursor/mcp.json",
            shape: McpShape::McpServersJson,
        }),
        hooks: Some(HookTarget {
            path: ".cursor/hooks.json",
            format: HookFormat::FlatJson,
        }),
        injection: Injection::CursorJson,
        compact_injection: false,
        events: es::ALL,
        legacy_id: Some("cursor"),
        event_map: CURSOR_EVENTS,
    },
    HarnessQuirk {
        id: "gemini-cli",
        display: "Gemini CLI",
        tier: Tier::A,
        detect: &[".gemini"],
        detect_cmds: &["gemini"],
        instruction_file: Some(".gemini/GEMINI.md"),
        mcp: None,
        hooks: Some(HookTarget {
            path: ".gemini/settings.json",
            format: HookFormat::NestedJson,
        }),
        injection: Injection::StdoutText,
        compact_injection: false,
        events: es::ALL & !es::STOP & !es::SUBAGENT,
        legacy_id: Some("gemini"),
        event_map: GEMINI_EVENTS,
    },
    HarnessQuirk {
        id: "kimi-code",
        display: "Kimi Code",
        tier: Tier::A,
        detect: &[".kimi-code"],
        detect_cmds: &["kimi"],
        instruction_file: None,
        mcp: Some(McpTarget {
            path: ".kimi-code/mcp.json",
            shape: McpShape::McpServersJson,
        }),
        hooks: Some(HookTarget {
            path: ".kimi-code/config.toml",
            format: HookFormat::TomlHooks,
        }),
        injection: Injection::UserPromptSubmit,
        compact_injection: false,
        events: es::ALL,
        legacy_id: Some("kimi-code"),
        event_map: KIMI_CODE_EVENTS,
    },
    HarnessQuirk {
        id: "kimi",
        display: "Kimi",
        tier: Tier::A,
        detect: &[".kimi"],
        detect_cmds: &["kimi"],
        instruction_file: None,
        mcp: Some(McpTarget {
            path: ".kimi/mcp.json",
            shape: McpShape::McpServersJson,
        }),
        hooks: Some(HookTarget {
            path: ".kimi/config.toml",
            format: HookFormat::TomlHooks,
        }),
        injection: Injection::StdoutText,
        compact_injection: false,
        events: es::ALL,
        legacy_id: Some("kimi"),
        event_map: KIMI_CODE_EVENTS,
    },
    HarnessQuirk {
        id: "devin",
        display: "Devin CLI",
        tier: Tier::A,
        detect: &[".devin"],
        detect_cmds: &["devin"],
        instruction_file: None,
        mcp: None,
        hooks: Some(HookTarget {
            path: ".devin/hooks.v1.json",
            format: HookFormat::NestedJson,
        }),
        injection: Injection::StdoutJson,
        compact_injection: false,
        events: es::ALL & !es::SUBAGENT & !es::PRE_COMPACT,
        legacy_id: None,
        event_map: DEVIN_EVENTS,
    },
    HarnessQuirk {
        id: "openclaw",
        display: "OpenClaw",
        tier: Tier::A,
        detect: &[".openclaw"],
        detect_cmds: &["openclaw"],
        instruction_file: None,
        mcp: None,
        hooks: Some(HookTarget {
            // OpenClaw discovers global plugins from ~/.openclaw/extensions/
            // (NOT plugins/ — that path is invisible to the loader).
            path: ".openclaw/extensions/stateroot",
            format: HookFormat::NativePlugin,
        }),
        injection: Injection::McpPull,
        compact_injection: false,
        events: es::ALL,
        legacy_id: None,
        event_map: OPENCLAW_EVENTS,
    },
    HarnessQuirk {
        id: "grok",
        display: "Grok Build CLI",
        tier: Tier::A,
        detect: &[".grok"],
        detect_cmds: &["grok"],
        instruction_file: None,
        mcp: None,
        hooks: Some(HookTarget {
            path: ".grok/hooks/stateroot.json",
            format: HookFormat::NestedJson,
        }),
        injection: Injection::McpPull,
        compact_injection: false,
        events: es::ALL,
        legacy_id: None,
        event_map: GROK_EVENTS,
    },
    HarnessQuirk {
        id: "zero",
        display: "Zero",
        tier: Tier::A,
        detect: &[".config/zero"],
        detect_cmds: &["zero"],
        instruction_file: None,
        mcp: None,
        hooks: Some(HookTarget {
            path: ".config/zero/hooks.json",
            format: HookFormat::ZeroExecJson,
        }),
        injection: Injection::McpPull,
        compact_injection: false,
        events: es::ALL & !es::USER_PROMPT_SUBMIT & !es::PRE_COMPACT,
        legacy_id: None,
        event_map: ZERO_EVENTS,
    },
    HarnessQuirk {
        id: "antigravity",
        display: "Antigravity CLI",
        tier: Tier::A,
        detect: &[".gemini/config"],
        detect_cmds: &["antigravity", "agy"],
        instruction_file: None,
        mcp: None,
        hooks: Some(HookTarget {
            path: ".gemini/config/hooks.json",
            format: HookFormat::NamedGroupsJson,
        }),
        injection: Injection::StdoutText,
        compact_injection: false,
        events: es::ALL
            & !es::SUBAGENT
            & !es::SESSION_END
            & !es::PRE_COMPACT
            & !es::USER_PROMPT_SUBMIT,
        legacy_id: None,
        event_map: ANTIGRAVITY_EVENTS,
    },
    HarnessQuirk {
        id: "opencode",
        display: "OpenCode",
        tier: Tier::B,
        detect: &[".config/opencode"],
        detect_cmds: &["opencode"],
        instruction_file: None,
        mcp: Some(McpTarget {
            path: ".config/opencode/opencode.json",
            shape: McpShape::McpServersJson,
        }),
        hooks: None,
        injection: Injection::StdoutText,
        compact_injection: false,
        events: es::ALL,
        legacy_id: Some("opencode"),
        event_map: TIER_B_EVENTS,
    },
    HarnessQuirk {
        id: "omp",
        display: "omp",
        tier: Tier::B,
        detect: &[".omp"],
        detect_cmds: &["omp"],
        instruction_file: None,
        mcp: None,
        hooks: None,
        injection: Injection::StdoutText,
        compact_injection: false,
        events: es::ALL,
        legacy_id: None,
        event_map: TIER_B_EVENTS,
    },
    HarnessQuirk {
        id: "pi",
        display: "pi",
        tier: Tier::B,
        detect: &[".pi/agent", ".pi"],
        detect_cmds: &["pi"],
        instruction_file: None,
        mcp: None,
        hooks: None,
        injection: Injection::StdoutText,
        compact_injection: false,
        events: es::ALL,
        legacy_id: None,
        event_map: PI_EVENTS,
    },
    HarnessQuirk {
        id: "vscode-copilot",
        display: "VS Code Copilot",
        tier: Tier::C,
        detect: &[".vscode"],
        detect_cmds: &["code"],
        instruction_file: Some(".github/copilot-instructions.md"),
        mcp: Some(McpTarget {
            path: ".vscode/mcp.json",
            shape: McpShape::ServersJson,
        }),
        hooks: None,
        // Instruction file carries the digest protocol; MCP tools remain.
        injection: Injection::None,
        compact_injection: false,
        events: 0,
        legacy_id: None,
        event_map: &[],
    },
    HarnessQuirk {
        id: "crush",
        display: "Crush",
        tier: Tier::C,
        detect: &[".config/crush"],
        detect_cmds: &["crush"],
        instruction_file: Some(".config/crush/STATEROOT.md"),
        mcp: None,
        hooks: None,
        injection: Injection::None,
        compact_injection: false,
        events: 0,
        legacy_id: None,
        event_map: &[],
    },
    HarnessQuirk {
        id: "hermes",
        display: "Hermes Agent",
        tier: Tier::A,
        detect: &[".hermes"],
        detect_cmds: &["hermes"],
        // SOUL.md is the hermes persona file (`agent/prompt_builder.py`
        // loads `HERMES_HOME/SOUL.md` at prompt-build time).
        instruction_file: Some(".hermes/SOUL.md"),
        mcp: Some(McpTarget {
            path: ".hermes/config.yaml",
            shape: McpShape::YamlMcpServers,
        }),
        // No hooks in v1: hermes has a plugin system and a stateroot plugin
        // is a later item. Resume works today via the MCP bridge (pull).
        hooks: None,
        injection: Injection::McpPull,
        compact_injection: false,
        events: 0,
        legacy_id: None,
        event_map: &[],
    },
];

/// Look up a quirk by canonical id.
pub fn quirk(id: &str) -> Option<&'static HarnessQuirk> {
    ADAPTERS.iter().find(|q| q.id == id)
}

/// Look up a quirk by legacy CLI id (pre-registry ids like `claude`, `gemini`).
pub fn quirk_by_legacy_id(legacy: &str) -> Option<&'static HarnessQuirk> {
    ADAPTERS.iter().find(|q| q.legacy_id == Some(legacy))
}

/// Look up a quirk by any id (canonical first, then legacy).
pub fn quirk_any(id: &str) -> Option<&'static HarnessQuirk> {
    let normalized = crate::skill_federation::normalize_harness(id);
    quirk(id).or_else(|| quirk_by_legacy_id(id)).or_else(|| {
        ADAPTERS.iter().find(|adapter| {
            crate::skill_federation::normalize_harness(adapter.id) == normalized
                || adapter
                    .legacy_id
                    .map(crate::skill_federation::normalize_harness)
                    .as_deref()
                    == Some(normalized.as_str())
        })
    })
}

/// All native integration adapters.
pub fn adapters() -> &'static [HarnessQuirk] {
    ADAPTERS
}

/// True when the harness is detected under `home`.
pub fn quirk_detected(home: &Path, quirk: &HarnessQuirk) -> bool {
    super::paths::quirk_detected(home, quirk)
}

/// Normalize a harness-native event name to the canonical vocabulary.
/// Returns `None` for unknown events (callers treat them as no-ops).
pub fn normalize_event(quirk: &HarnessQuirk, event: &str) -> Option<&'static str> {
    let lowered = event.trim();
    for (harness_event, canonical) in quirk.event_map {
        if harness_event.eq_ignore_ascii_case(lowered) {
            return Some(canonical);
        }
    }
    // Passthrough for already-canonical names (works for any harness).
    CANONICAL_EVENTS
        .iter()
        .find(|canonical| lowered == **canonical)
        .copied()
}

/// Canonical event vocabulary.
pub const CANONICAL_EVENTS: &[&str] = &[
    "session_start",
    "user_prompt_submit",
    "pre_tool_use",
    "post_tool_use",
    "tool_failure",
    "pre_compact",
    "post_compaction",
    "stop",
    "session_end",
    "subagent_start",
    "subagent_stop",
    "notification",
];

/// Event kinds driving the hook command's behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// Resume events: print the hook digest to stdout.
    Resume,
    /// Capture events: append a sanitized observation to the spool.
    Capture,
    /// Checkpoint events: checkpoint from the spool tail (+handoff on stop/session_end).
    Checkpoint,
}

/// Classify a canonical event.
pub fn event_kind(canonical: &str) -> Option<EventKind> {
    match canonical {
        // Resume only on session_start; user_prompt_submit captures corrections.
        // kimi-code is the exception — hook.rs also calls resume on prompt-submit
        // when injection is UserPromptSubmit (SessionStart stdout is discarded).
        "session_start" => Some(EventKind::Resume),
        "user_prompt_submit" => Some(EventKind::Capture),
        "pre_tool_use" | "post_tool_use" | "notification" | "subagent_start" | "subagent_stop" => {
            Some(EventKind::Capture)
        }
        "tool_failure" | "pre_compact" | "post_compaction" | "stop" | "session_end" => {
            Some(EventKind::Checkpoint)
        }
        _ => None,
    }
}

/// Home-relative path helper (registry paths are home-relative).
pub fn quirk_path(home: &Path, rel: &str) -> PathBuf {
    home.join(rel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_17_unique_rows() {
        assert_eq!(ADAPTERS.len(), 17);
        let mut ids: Vec<&str> = ADAPTERS.iter().map(|q| q.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 17, "duplicate ids in registry");
        for q in ADAPTERS {
            assert!(!q.display.is_empty(), "{}: empty display", q.id);
            assert!(!q.detect.is_empty(), "{}: no detect markers", q.id);
            assert!(!q.detect_cmds.is_empty(), "{}: no detect commands", q.id);
            match q.tier {
                // hermes is the one Tier A row without hooks in v1 (its
                // plugin system integration is a later item; it still has a
                // full block+MCP install path).
                Tier::A => assert!(
                    q.hooks.is_some() || q.id == "hermes",
                    "{}: Tier A without hooks",
                    q.id
                ),
                Tier::B | Tier::C => {}
            }
            // Every event_map canonical target is a known canonical event.
            for (_, canonical) in q.event_map {
                assert!(
                    CANONICAL_EVENTS.contains(canonical),
                    "{}: unknown canonical event {canonical}",
                    q.id
                );
            }
        }
    }

    #[test]
    fn hermes_row_shape() {
        let hermes = quirk("hermes").expect("hermes row");
        assert_eq!(hermes.tier, Tier::A);
        assert_eq!(hermes.detect, &[".hermes"]);
        assert_eq!(hermes.detect_cmds, &["hermes"]);
        assert_eq!(hermes.instruction_file, Some(".hermes/SOUL.md"));
        let mcp = hermes.mcp.expect("hermes mcp target");
        assert_eq!(mcp.path, ".hermes/config.yaml");
        assert_eq!(mcp.shape, McpShape::YamlMcpServers);
        assert!(hermes.hooks.is_none(), "hermes hooks are a later item");
    }

    #[test]
    fn legacy_ids_cover_the_original_seven() {
        for legacy in [
            "claude",
            "codex",
            "cursor",
            "gemini",
            "kimi-code",
            "kimi",
            "opencode",
        ] {
            assert!(
                quirk_by_legacy_id(legacy).is_some(),
                "missing legacy id {legacy}"
            );
        }
    }

    #[test]
    fn event_normalization() {
        let claude = quirk("claude-code").expect("claude");
        assert_eq!(
            normalize_event(claude, "SessionStart"),
            Some("session_start")
        );
        assert_eq!(
            normalize_event(claude, "UserPromptSubmit"),
            Some("user_prompt_submit")
        );
        let cursor = quirk("cursor").expect("cursor");
        assert_eq!(
            normalize_event(cursor, "beforeSubmitPrompt"),
            Some("user_prompt_submit")
        );
        assert_eq!(
            normalize_event(cursor, "postToolUseFailure"),
            Some("tool_failure")
        );
        let gemini = quirk("gemini-cli").expect("gemini");
        assert_eq!(normalize_event(gemini, "PreCompress"), Some("pre_compact"));
        let pi = quirk("pi").expect("pi");
        assert_eq!(
            normalize_event(pi, "before_agent_start"),
            Some("user_prompt_submit")
        );
        assert_eq!(normalize_event(pi, "tool_call"), Some("pre_tool_use"));
        assert_eq!(normalize_event(pi, "agent_end"), Some("stop"));
        // Passthrough canonical names work everywhere.
        assert_eq!(normalize_event(gemini, "stop"), Some("stop"));
        assert_eq!(normalize_event(claude, "NotARealEvent"), None);
    }

    #[test]
    fn event_kinds() {
        assert_eq!(event_kind("session_start"), Some(EventKind::Resume));
        assert_eq!(event_kind("user_prompt_submit"), Some(EventKind::Capture));
        assert_eq!(event_kind("post_tool_use"), Some(EventKind::Capture));
        assert_eq!(event_kind("tool_failure"), Some(EventKind::Checkpoint));
        assert_eq!(event_kind("stop"), Some(EventKind::Checkpoint));
        assert_eq!(event_kind("nonsense"), None);
    }

    #[test]
    fn vscode_copilot_uses_servers_key() {
        let copilot = quirk("vscode-copilot").expect("copilot");
        let mcp = copilot.mcp.expect("mcp target");
        assert_eq!(mcp.shape, McpShape::ServersJson);
    }

    #[test]
    fn every_adapter_declares_a_truthful_delivery_policy() {
        for quirk in ADAPTERS {
            let policy = quirk.delivery();
            assert_eq!(
                policy,
                DigestDeliveryPolicy::for_id(quirk.id),
                "{}: delivery() must be explicit",
                quirk.id
            );
            match quirk.id {
                "kimi-code" => {
                    assert_eq!(policy.primary_event, "user_prompt_submit");
                    assert!(!policy.session_start_prints);
                    assert!(policy.prompt_submit_injects);
                    assert_eq!(policy.tier, DeliveryTier::Automatic);
                }
                "cursor" => {
                    // Cursor starts on sessionStart and restores an armed
                    // compaction re-anchor through postToolUse.
                    assert!(policy.session_start_prints);
                    assert!(policy.session_start_marks);
                    assert!(!policy.prompt_submit_injects);
                    assert!(policy.note.contains("postToolUse"));
                    assert_eq!(policy.tier, DeliveryTier::Automatic);
                }
                "openclaw" | "opencode" | "omp" => {
                    assert!(policy.prompt_submit_injects);
                    assert!(!policy.session_start_marks);
                    assert_eq!(policy.tier, DeliveryTier::Automatic);
                }
                "pi" => {
                    assert_eq!(policy.primary_event, "user_prompt_submit");
                    assert!(policy.session_start_prints);
                    assert!(!policy.session_start_marks);
                    assert!(policy.prompt_submit_injects);
                    assert_eq!(policy.tier, DeliveryTier::Automatic);
                    assert!(
                        policy.note.contains("before_agent_start"),
                        "pi note must name the verified injection event"
                    );
                }
                "hermes" | "vscode-copilot" | "crush" | "zero" => {
                    assert_eq!(policy.tier, DeliveryTier::Degraded);
                    assert!(!policy.prompt_submit_injects);
                }
                "claude-code" | "codex" | "kimi" | "devin" => {
                    assert!(policy.session_start_marks);
                    assert!(policy.prompt_submit_injects);
                    assert_eq!(policy.tier, DeliveryTier::Automatic);
                }
                "gemini-cli" | "antigravity" => {
                    assert!(policy.session_start_marks);
                    assert!(!policy.prompt_submit_injects);
                    assert_eq!(policy.tier, DeliveryTier::Automatic);
                }
                "grok" => {
                    assert!(!policy.session_start_marks);
                    assert!(policy.prompt_submit_injects);
                }
                other => panic!("add a delivery assertion for {other}"),
            }
        }
    }
}
