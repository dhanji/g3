# Workspace Memory
> Updated: 2026-09-02T11:28:53Z | Size: 30.0k chars

### Remember Tool Wiring
- `crates/g3-core/src/tools/memory.rs` [0..5686]
  - `get_memory_path()` [486] - resolves `analysis/memory.md`
  - `execute_remember()` [1066] - tool handler
  - `merge_memory()` [2324] - merges new notes into existing
- `crates/g3-core/src/tool_definitions.rs` [956..] - remember tool in `create_core_tools()`
- `crates/g3-core/src/tool_dispatch.rs` [670] - dispatch case
- `crates/g3-core/src/prompts.rs` [4200..6500] - Workspace Memory prompt section
- `crates/g3-cli/src/project_files.rs` - `read_workspace_memory()` loads `analysis/memory.md`

### Context Window & Compaction
- `crates/g3-core/src/context_window.rs` [0..43282]
  - `ThinResult` [765] - scope, before/after %, chars_saved
  - `ContextWindow` [2220] - token tracking, message history
  - `add_message_with_tokens()` [3171] - preserves messages with `tool_calls` even if content empty
  - `estimate_message_tokens()` [7695] - sums content + tool_calls[].input tokens (chars/3 * 1.1 + 20 overhead)
  - `should_compact()` [8954] - threshold check (80%)
  - `reset_with_summary()` [10685] - compact history to summary
  - `reset_with_summary_and_stub()` [11120] - ACD integration
  - `extract_preserved_messages()` [13199] - strips `tool_calls` from last assistant to prevent orphaned `tool_use`
  - `thin_context()` [15038] - replace large results with file refs
- `crates/g3-core/src/compaction.rs` [0..11404]
  - `CompactionResult`, `CompactionConfig` - result/config structs
  - `perform_compaction()` - unified for force_compact() and auto-compaction
  - `calculate_capped_summary_tokens()`, `should_disable_thinking()`
  - `build_summary_messages()`, `apply_summary_fallback_sequence()`
  - ACD integration [195..240] - creates fragment+stub during compaction
- `crates/g3-core/src/lib.rs`
  - `force_compact()` [47902]
  - `stream_completion_with_tools()` [85389] - main agent loop

### Session Storage & Continuation
- `crates/g3-core/src/session_continuation.rs` [0..22907]
  - `SessionContinuation` [1024]
  - `save_continuation()` [5581]
  - `load_continuation()` [6428]
- `crates/g3-core/src/paths.rs` [0..5498]
  - `get_session_logs_dir()` [2434]
  - `get_thinned_dir()` [3060]
  - `get_fragments_dir()` [3295] - `.g3/sessions/<id>/fragments/`
  - `get_session_file()` [3517]
- `crates/g3-core/src/session.rs` - session logging utilities

### Tool System
- `crates/g3-core/src/tool_definitions.rs` [0..15391]
  - `ToolConfig` [381]
  - `create_tool_definitions()` [742]
  - `create_core_tools()` [956]
- `crates/g3-core/src/tool_dispatch.rs` [0..3983] - `dispatch_tool()` [670] routing

### CLI Module Structure
- `crates/g3-cli/src/lib.rs` [0..9309] - `run()` [1242], mode dispatch, config loading
- `crates/g3-cli/src/cli_args.rs` [0..6043] - `Cli` [1374] struct (clap)
- `crates/g3-cli/src/autonomous.rs` [0..25630] - `run_autonomous()` [638], coach-player loop
- `crates/g3-cli/src/agent_mode.rs` [0..13558] - `run_agent_mode()` [1000]
- `crates/g3-cli/src/accumulative.rs` [0..12006] - `run_accumulative_mode()` [796]
- `crates/g3-cli/src/interactive.rs` [0..19222] - `run_interactive()` [3809], REPL
- `crates/g3-cli/src/task_execution.rs` [0..5520] - `execute_task_with_retry()` [1069]
- `crates/g3-cli/src/commands.rs` [0..20115] - `/help`, `/compact`, `/thinnify`, `/fragments`, `/rehydrate`
- `crates/g3-cli/src/utils.rs` [0..6154] - `display_context_progress()`, `setup_workspace_directory()`, `load_config_with_cli_overrides()`
- `crates/g3-cli/src/display.rs` [0..12573] - `format_workspace_path()` [286], `LoadedContent`, `print_loaded_status()`

### Auto-Memory System
- `crates/g3-core/src/lib.rs`
  - `tool_calls_this_turn` [5272] - tracks tools per turn
  - `set_auto_memory()` [64643] - enable/disable
  - `send_auto_memory_reminder()` [72800] - MEMORY CHECKPOINT prompt
  - `execute_tool_in_dir()` [132582] - records tool calls
- `crates/g3-core/src/prompts.rs` [3800..4500] - Memory Format in system prompt
- `crates/g3-cli/src/lib.rs` - `--auto-memory` CLI flag

### Streaming Markdown Formatter
- `crates/g3-cli/src/streaming_markdown.rs` [0..37669]
  - `process_in_code_block()` [17159] - detects closing fence
  - `format_header()` [21339] - headers with inline formatting
  - `emit_code_block()` [27134] - joins buffer, highlights code
  - `flush_incomplete()` [28434] - handles unclosed blocks at stream end
- `crates/g3-cli/tests/streaming_markdown_test.rs` - header formatting tests
- **Gotcha**: closing ``` without trailing newline must be detected in `flush_incomplete()`

### Retry Infrastructure
- `crates/g3-core/src/retry.rs` [0..11865] - `execute_with_retry()`, `retry_operation()`, `RetryConfig`, `RetryResult`
- `crates/g3-cli/src/task_execution.rs` - `execute_task_with_retry()`

### UI Abstraction Layer
- `crates/g3-core/src/ui_writer.rs` [0..8007] - `UiWriter` trait [211], `NullUiWriter` [6538], `print_thin_result()` [1136]
- `crates/g3-cli/src/ui_writer_impl.rs` [0..14000] - `ConsoleUiWriter`, `print_tool_compact()`
- `crates/g3-cli/src/simple_output.rs` [0..1200] - `SimpleOutput` helper

### Feedback Extraction
- `crates/g3-core/src/feedback_extraction.rs` [0..22455] - `extract_coach_feedback()`, `try_extract_from_session_log()`, `try_extract_from_native_tool_call()`
- `crates/g3-cli/src/coach_feedback.rs` [0..4025] - `extract_from_logs()` for coach-player loop

### Streaming Utilities & State
- `crates/g3-core/src/streaming.rs` [0..27241]
  - `MAX_ITERATIONS` [419] - constant (400)
  - `StreamingState` [499] - cross-iteration: full_response, first_token_time, iteration_count
  - `ToolOutputFormat` [1606] - enum: SelfHandled, Compact(String), Regular
  - `format_tool_result_summary()` [1743], `is_compact_tool()` [2635], `format_compact_tool_summary()` [3179]
  - `IterationState` [5061] - per-iteration: parser, current_response, tool_executed
  - `log_stream_error()` [8017], `truncate_for_display()` [10887], `truncate_line()` [11247]
  - `is_connection_error()` [21620]

### Background Process Management
- `crates/g3-core/src/background_process.rs` [0..9048]
  - `BackgroundProcessManager` [1466], `start()` [2601], `list()` [5527], `get()` [5731], `is_running()` [5934], `remove()` [6462]
- No `stop()` method — use shell `kill <pid>`

### Unified Diff Application
- `crates/g3-core/src/utils.rs` [5000..15000] - `apply_unified_diff_to_string()`, `parse_unified_diff_hunks()`
- Handles multi-hunk diffs, CRLF normalization, range constraints

### Error Classification
- `crates/g3-core/src/error_handling.rs` [0..19454]
  - `ErrorType` [5206], `RecoverableError` [5465] (enum), `classify_error()` [5972]
- Priority: rate limit > network > server > busy > timeout > token limit > context length
- **Gotcha**: "Connection timeout" → NetworkError (not Timeout) due to "connection" keyword priority

### CLI Metrics
- `crates/g3-cli/src/metrics.rs` [0..5416] - `TurnMetrics`, `format_elapsed_time()`, `generate_turn_histogram()`

### ACD (Aggressive Context Dehydration)
Saves conversation fragments to disk, replaces with stubs.

- `crates/g3-core/src/acd.rs` [0..22830]
  - `Fragment` - `new()`, `save()`, `load()`, `generate_stub()`, `list_fragments()`, `get_latest_fragment_id()`
- `crates/g3-core/src/tools/acd.rs` [0..8500] - `execute_rehydrate()` tool
- `crates/g3-cli/src/lib.rs` - `--acd` flag; `/fragments`, `/rehydrate` commands
- **Fragment JSON**: `fragment_id`, `created_at`, `messages`, `message_count`, `user_message_count`, `assistant_message_count`, `tool_call_summary`, `estimated_tokens`, `topics`, `preceding_fragment_id`

### UTF-8 Safe String Slicing
Rust `&s[..n]` panics on multi-byte chars (emoji, CJK) if sliced mid-character.
**Pattern**: `s.char_indices().nth(n).map(|(i,_)| i).unwrap_or(s.len())`
**Danger zones**: Display truncation, ACD stubs, user input, non-ASCII text.

### Studio - Multi-Agent Workspace Manager
- `crates/studio/src/main.rs` [0..12500] - `cmd_run()`, `cmd_status()`, `cmd_accept()`, `cmd_discard()`, `extract_session_summary()`
- `crates/studio/src/session.rs` - `Session`, `SessionStatus`
- `crates/studio/src/git.rs` - `GitWorktree` for isolated agent sessions
- **Session log**: `<worktree>/.g3/sessions/<session_id>/session.json`
- **Fields**: `context_window.{conversation_history, percentage_used, total_tokens, used_tokens}`, `session_id`, `status`, `timestamp`

### Racket Code Search Support
- `crates/g3-core/src/code_search/searcher.rs`
  - Racket parser [~45] - `tree_sitter_racket::LANGUAGE`
  - Extensions [~90] - `.rkt`, `.rktl`, `.rktd` → "racket"

### Language-Specific Prompt Injection
Auto-detects languages and injects toolchain guidance.

- `crates/g3-cli/src/language_prompts.rs`
  - `LANGUAGE_PROMPTS` - (lang_name, extensions, prompt_content)
  - `AGENT_LANGUAGE_PROMPTS` - (agent_name, lang_name, prompt_content)
  - `detect_languages()` - scans workspace
  - `scan_directory_for_extensions()` - recursive, depth 2, skips hidden/vendor
  - `get_language_prompts_for_workspace()`, `get_agent_language_prompts_for_workspace()`
- `crates/g3-cli/src/agent_mode.rs` - appends agent-specific prompts
- `prompts/langs/` - language prompt files
- **To add language**: Create `prompts/langs/<lang>.md`, add to `LANGUAGE_PROMPTS`
- **To add agent+lang**: Create `prompts/langs/<agent>.<lang>.md`, add to `AGENT_LANGUAGE_PROMPTS`

### MockProvider for Testing
- `crates/g3-providers/src/mock.rs`
  - `MockProvider` [220..320] - response queue, request tracking
  - `MockResponse` [35..200] - configurable chunks and usage
  - `scenarios` module [410..480] - `text_only_response()`, `multi_turn()`, `tool_then_response()`
- `crates/g3-core/tests/mock_provider_integration_test.rs` - integration tests
- **Usage**: `MockProvider::new().with_response(MockResponse::text("Hello!"))`

### G3 Status Message Formatting
- `crates/g3-cli/src/g3_status.rs`
  - `Status` [12] - enum: Done, Failed, Error, Custom, Resolved, Insufficient, NoChanges
  - `G3Status` [44] - static methods for "g3:" prefixed messages
  - `progress()` [48], `done()` [72], `failed()` [81], `thin_result()` [236]

### Prompt Cache Statistics
- `crates/g3-providers/src/lib.rs` - `Usage.cache_creation_tokens` [6780], `cache_read_tokens` [6929]
- `crates/g3-providers/src/anthropic.rs` - parses `cache_creation_input_tokens`, `cache_read_input_tokens`
- `crates/g3-providers/src/openai.rs` - parses `prompt_tokens_details.cached_tokens`
- `crates/g3-core/src/lib.rs` - `CacheStats` [3066]; `Agent.cache_stats`
- `crates/g3-core/src/stats.rs` [189..230] - `format_cache_stats()` with hit rate metrics

### Embedded Provider (Local LLM)
Local inference via llama-cpp-rs with Metal acceleration.

- `crates/g3-providers/src/embedded.rs`
  - `EmbeddedProvider` [22..85] - session, model_name, max_tokens, temperature, context_length
  - `new()` [26..85] - tilde expansion, auto-downloads Qwen if missing
  - `format_messages()` [87..175] - converts to prompt string (Qwen/Mistral/Llama templates)
  - `get_stop_sequences()` [280..340] - model-specific stop tokens
  - `stream()` [560..780] - via spawn_blocking + mpsc

### Chat Template Formats
| Model | Start Token | End Token |
|-------|-------------|----------|
| Qwen | `<\|im_start\|>role\n` | `<\|im_end\|>` |
| GLM-4 | `[gMASK]<sop><\|role\|>\n` | `<\|endoftext\|>` |
| Mistral | `<s>[INST]` | `[/INST]` |
| Llama | `<<SYS>>` | `<</SYS>>` |

### Recommended GGUF Models
| Model | Size | Use Case |
|-------|------|----------|
| GLM-4-9B-Q8_0 | ~10GB | Fast, capable |
| GLM-4-32B-Q6_K_L | ~27GB | Top tier coding/reasoning |
| Qwen3-4B-Q4_K_M | ~2.3GB | Small, rivals 72B |

**Download**: `huggingface-cli download <repo> --include "<file>" --local-dir ~/.g3/models/`

**Config**:
```toml
[providers.embedded.glm4]
model_path = "~/.g3/models/THUDM_GLM-4-32B-0414-Q6_K_L.gguf"
model_type = "glm4"
context_length = 32768
max_tokens = 4096
gpu_layers = 99
```

### Agent Skills System
Portable skill packages with SKILL.md + optional scripts per Agent Skills spec (agentskills.io).

- `crates/g3-core/src/skills/mod.rs` [0..1501] - exports: `Skill`, `discover_skills`, `generate_skills_prompt`
- `crates/g3-core/src/skills/parser.rs` [0..10750]
  - `Skill` [389] - name, description, metadata, body, path
  - `Skill::parse()` [1632] - parses SKILL.md with YAML frontmatter
  - `validate_name()` [4970] - 1-64 chars, lowercase+hyphens
- `crates/g3-core/src/skills/discovery.rs` [0..12921]
  - `discover_skills()` [1266] - scans 5 locations: embedded → global → extra → workspace → repo
  - `load_embedded_skills()` [3263] - synthetic path `<embedded:name>/SKILL.md`
- `crates/g3-core/src/skills/embedded.rs` [0..1674]
  - `EmbeddedSkill` [574] - name, skill_md
  - `EMBEDDED_SKILLS` [944] - static array (currently empty)
- `crates/g3-core/src/skills/prompt.rs` [0..5628]
  - `generate_skills_prompt()` [397] - generates `<available_skills>` XML
- `crates/g3-config/src/lib.rs` [180..200] - `SkillsConfig` (enabled, extra_paths)
- `crates/g3-cli/src/project_files.rs` - `discover_and_format_skills()`

**Skill Locations** (priority: later overrides earlier):
1. Embedded (compiled in)
2. `~/.g3/skills/` (global)
3. Config extra_paths
4. `.g3/skills/` (workspace)
5. `skills/` (repo root)

**SKILL.md Format**:
```yaml
---
name: skill-name          # Required: 1-64 chars, lowercase + hyphens
description: What it does # Required: 1-1024 chars
license: Apache-2.0       # Optional
compatibility: Requires X # Optional
---
# Instructions...
```

### Research Tool (First-Class)
Async web research via background scout agent.

- `crates/g3-core/src/pending_research.rs` [0..18348]
  - `ResearchStatus` [682] - Pending/Complete/Failed
  - `ResearchTask` [1273] - task state
  - `PendingResearchManager` [2906] - thread-safe tracking with Arc<RwLock>
  - `with_notifications()` [3749] - broadcast channel for interactive mode
  - `register()` [5069], `complete()` [5480], `fail()` [6419], `get()` [7344], `list_pending()` [7806], `take_completed()` [8952]
- `crates/g3-core/src/tools/research.rs` [0..17060]
  - `CONTEXT_ERROR_PATTERNS` [929] - detects context window exhaustion
  - `execute_research()` [1644] - spawns scout agent in background tokio task
  - `execute_research_status()` [7540] - check pending/completed
  - `extract_report()` [10694], `strip_ansi_codes()` [13148]
- `crates/g3-core/src/lib.rs`
  - `inject_completed_research()` [31375] - injects results as user messages
  - `enable_research_notifications()` [33459] - for interactive mode
- **Tools**: `research` (async, returns research_id), `research_status` (check pending)

### Plan Mode
Structured task planning with cognitive forcing — requires happy/negative/boundary checks.

- `crates/g3-core/src/tools/plan.rs` [0..49798]
  - `PlanState` [1044] - enum: Todo, Doing, Done, Blocked
  - `Checks` [2823] - happy, negative[], boundary[]
  - `PlanItem` [4021] - id, description, state, touches, checks, evidence, notes
  - `Plan` [6498] - plan_id, revision, approved_revision, items[]
  - `EvidenceType` [9578] - CodeLocation, TestReference, Unknown
  - `VerificationStatus` [10133] - Verified, Warning, Error, Skipped
  - `parse_evidence()` [12712] - parses `file:line-line` or `file::test_name`
  - `verify_code_location()` [14888] - checks file exists, lines in range
  - `verify_test_reference()` [16733] - checks test file, searches for fn
  - `get_plan_path()` [18655] - `.g3/sessions/<id>/plan.g3.md`
  - `read_plan()` [18818], `write_plan()` [19277] - YAML in markdown
  - `plan_verify()` [21978] - verifies evidence when complete; checks envelope existence
  - `format_verification_results()` [23395] - takes `working_dir: Option<&Path>` as third param
  - `execute_plan_read()` [25881], `execute_plan_write()` [27233], `execute_plan_approve()` [30651]
- `crates/g3-core/src/tool_definitions.rs` [263..330] - plan_read, plan_write, plan_approve
- `crates/g3-core/src/prompts.rs` [21..130] - SHARED_PLAN_SECTION
- **Tool names**: `plan_read`, `plan_write`, `plan_approve` (underscores, not dots)
- **Evidence formats**: `src/foo.rs:42-118`, `src/foo.rs:42`, `tests/foo.rs::test_bar`

### Invariants System (Rulespec & Envelope)
Machine-readable invariants for Plan Mode verification. Rulespec read from `analysis/rulespec.yaml` (checked-in).

- `crates/g3-core/src/tools/invariants.rs` [0..73975]
  - `Claim` [2024] - name + selector
  - `PredicateRule` [3009] - Contains, Equals, Exists, NotExists, GreaterThan, LessThan, MinLength, MaxLength, Matches
  - `Predicate` [5617] - claim, rule, value, source, notes
  - `Rulespec` [8734] - claims[] + predicates[]
  - `ActionEnvelope` [11203] - facts HashMap
  - `Selector` [12900] - XPath-like: `foo.bar`, `foo[0]`, `foo[*]`
  - `read_rulespec()` [29472] - takes `&Path` (working_dir)
  - `evaluate_rulespec()` [32056] - evaluates against envelope

### Write Envelope Tool
- `crates/g3-core/src/tools/envelope.rs` [0..23347]
  - `execute_write_envelope()` [8764] - parses YAML facts, writes envelope.yaml, calls verify_envelope()
  - `verify_envelope()` [11705] - compiles rulespec on-the-fly, extracts facts, runs datalog, writes `.dl` + `datalog_evaluation.txt` (shadow mode)
- `crates/g3-core/src/tool_definitions.rs` [266..282] - write_envelope tool definition
- `crates/g3-core/src/tool_dispatch.rs` - write_envelope dispatch case
- **Workflow**: `write_envelope` → `verify_envelope()` → datalog shadow, then `plan_write(done)` → `plan_verify()` → checks envelope exists

### Datalog Invariant Verification
- `crates/g3-core/src/tools/datalog.rs` [0..80172]
  - `CompiledPredicate` [1681] - id, claim_name, selector, rule, expected_value, source, notes
  - `CompiledRulespec` [2728] - plan_id, compiled_at_revision, predicates, claims
  - `compile_rulespec()` [3588] - validates selectors, builds claim lookup
  - `Fact` [6741] - claim_name, value
  - `extract_facts()` [7057] - uses Selector to navigate envelope YAML; fallback wraps in `facts:` if selector has `facts.` prefix
  - `extract_values_recursive()` [8478] - handles arrays/objects/scalars, adds __length facts
  - `DatalogPredicateResult` [10308], `DatalogExecutionResult` [10862]
  - `execute_rules()` [11627] - builds fact lookup, uses datafrog Iteration; when conditions delegate to `evaluate_predicate_datalog()`
  - `evaluate_predicate_datalog()` [14872] - handles all 9 PredicateRule types
  - `escape_datalog_string()` [23990], `format_datalog_program()` [24582] - Soufflé-style .dl output
  - `format_datalog_results()` [31136] - formats for shadow mode display
- **Relations**: `claim_value(claim, value)`, `claim_length(claim, length)`, `predicate_pass(id)`, `predicate_fail(id)`

### Solon Agent (Rulespec Authoring)
- `agents/solon.md` - interactive rulespec authoring agent prompt
- `crates/g3-cli/src/embedded_agents.rs` [551] - 9 embedded agents: breaker, carmack, euler, fowler, hopper, huffman, lamport, scout, solon
- **Usage**: `g3 --agent solon`

### Structured Tool Call Messages
Native tool calls stored as structured `MessageToolCall` objects, not inline JSON text.

- `crates/g3-providers/src/lib.rs` [0..17486]
  - `MessageToolCall` [2894] - id, name, input
  - `Message` [3014] - `tool_calls: Vec<MessageToolCall>`, `tool_result_id: Option<String>`
- `crates/g3-providers/src/anthropic.rs` [0..74631]
  - `convert_messages()` [8642] - emits `tool_use`/`tool_result` blocks for structured tool calls
  - `strip_orphaned_tool_use()` [14737] - defense-in-depth: strips orphaned `tool_use` blocks with no matching `tool_result`
  - `ToolResultContent` [46268] - enum (Text | Blocks) for structured content
  - `ToolResultBlock` [46650] - enum (Image, Text) inside tool_result; images from read_image nested here, not as top-level blocks
- `crates/g3-core/src/lib.rs` - `ToolCall.id` [2516] field from native providers
- `crates/g3-core/src/streaming_parser.rs` [0..29244] - `process_chunk()` [10449] preserves tool call `id`
- **Gotcha**: Images in tool result messages must be nested inside `tool_result.content` array, not as top-level `Image` blocks (Anthropic API rejects mixed top-level Image+ToolResult)

### Tool Call Token Tracking
- `crates/g3-core/src/context_window.rs` - `estimate_message_tokens()` [7695] accounts for `tool_calls[].input`
- Token formula: content_tokens + per-tool (input_chars/3 * 1.1 + 20 overhead)
- **Gotcha**: Without this, tool input JSON is invisible to tracker → compaction never triggers → API 400

### Studio SDLC Pipeline
Orchestrates 7 agents in sequence for codebase maintenance.

- `crates/studio/src/sdlc.rs`
  - `PIPELINE_STAGES` [28..62] - euler → breaker → hopper → fowler → carmack → lamport → huffman
  - `Stage` [18..26], `StageStatus` [65..80] - Pending, Running, Complete, Failed, Skipped
  - `PipelineState` [108..140] - run_id, stages[], commit_cursor, session_id
  - `display_pipeline()` [354..390] - box display with status icons
- `crates/studio/src/main.rs`
  - `cmd_sdlc_run()` [540..655] - orchestrates pipeline, merges on completion
  - `has_commits_on_branch()` [715..728] - counts commits ahead of main
- `crates/studio/src/git.rs` - `merge_to_main()` (hardcodes 'main')
- **State**: `.g3/sdlc/pipeline.json`
- **CLI**: `studio sdlc run [-c N]`, `studio sdlc status`, `studio sdlc reset`

### Terminal Width Responsive Output
Tool output responsive to terminal width — no line wrapping, 4-char right margin.

- `crates/g3-cli/src/terminal_width.rs`
  - `get_terminal_width()` [21..28] - usable width (terminal - 4), min 40, default 80
  - `clip_line()` [33..44] - clips with "…", UTF-8 safe
  - `compress_path()` [53..96] - preserves filename, truncates dirs from left
  - `compress_command()` [101..103] - clips from right
  - `available_width_after_prefix()` [115..117]
- `crates/g3-cli/src/ui_writer_impl.rs`
  - `print_tool_output_header()` [293..410] - uses compress_path/compress_command
  - `update_tool_output_line()` [407..445], `print_tool_output_line()` [447..454] - clip_line()
  - `print_tool_compact()` [475..635] - width-aware compact display

### Plan Approval Gate (Non-Destructive + Baseline-Aware)
- `crates/g3-core/src/tools/plan.rs` [973..983] - `ApprovalGateResult` enum: `Allowed`, `Blocked { message }`, `NotGitRepo` — no `reverted_files` field
- `crates/g3-core/src/tools/plan.rs` [985..1003] - `get_dirty_files()` - returns `HashSet<String>` of dirty file paths from `git status --porcelain`
- `crates/g3-core/src/tools/plan.rs` [1005..1098] - `check_plan_approval_gate(session_id, working_dir, baseline_dirty)` - warn-only, never reverts/deletes files, excludes baseline dirty files
- `crates/g3-core/src/lib.rs` [170..171] - `baseline_dirty_files: HashSet<String>` field on Agent
- `crates/g3-core/src/lib.rs` [1675..1686] - `set_plan_mode(enabled, working_dir)` - captures baseline on enable, clears on disable
- **Key invariant**: The approval gate NEVER deletes or reverts files. It only warns.
- **Key invariant**: Pre-existing dirty files (captured at plan mode start) are excluded from gate checks.

### Context Window Calibration (Token Drift Fix)
- `crates/g3-core/src/context_window.rs` [159..189] - `update_usage_from_response()` now calibrates `used_tokens` from API `prompt_tokens` (ground truth). When `prompt_tokens > 0`, snaps `used_tokens` to it. When 0, leaves unchanged (heuristic fallback).
- `crates/g3-core/src/context_window.rs` [93..100] - No more 1% safety buffer. `total_tokens = raw` (was `raw * 0.99`).
- `crates/g3-core/src/context_window.rs` [222..250] - `estimate_message_tokens()` now adds: +4 per-message overhead, +30 per tool_use block (was 20), +15 per tool_result message.
- `crates/g3-core/src/lib.rs` [2232..2241] - `ensure_context_capacity()` called inside streaming loop for iteration > 1 (catches post-tool-execution growth).
- **Root cause**: Heuristic token estimation drifted ~48% over 809 messages / 388 tool calls (136k estimated vs 201k actual). API `prompt_tokens` is ground truth.

### Context Window Calibration (Token Drift Fix) - CORRECTED
- `crates/g3-core/src/context_window.rs` [168..189] - `update_usage_from_response()` calibrates `used_tokens` from API `prompt_tokens` (ground truth). When `prompt_tokens > 0`, snaps `used_tokens` to it. When 0, leaves unchanged (heuristic fallback).
- `crates/g3-core/src/lib.rs` [2316..2319] - Calibration call placed **inline** during streaming (when usage chunk arrives in `chunk.usage`), NOT after the streaming loop. Critical because text-only responses take an early return path that bypasses post-loop code.
- `crates/g3-core/src/lib.rs` [2892..2898] - Post-loop code only handles fallback (no-usage) case now.
- `crates/g3-core/src/context_window.rs` [87..93] - 1% safety buffer IS still in place (`total_tokens * 0.99`). Left as safety net between calibration points.
- **Root cause of display bug**: (1) `update_usage_from_response` never calibrated `used_tokens`, only `cumulative_tokens`. (2) `execute_single_task` had mock usage with hardcoded `prompt_tokens: 100`. (3) Post-loop usage update was bypassed by early returns in text-only response paths.
- **Key streaming flow**: For text-only responses (most common in interactive mode), `chunk.finished` triggers an early `return Ok(self.finalize_streaming_turn(...))` that bypasses all post-loop code. Calibration MUST happen inline when `chunk.usage` arrives.

### Anthropic Temperature Stripping (API Constraint)
Anthropic's API rejects the `temperature` field for newer models (e.g. extended-thinking / reasoning models like claude-sonnet-4-5).

- `crates/g3-providers/src/anthropic.rs`
  - `AnthropicRequest` struct [~895] - NO `temperature` field (must NEVER be re-added)
  - `create_request_body()` [559] - 5-arg signature, no temperature param
  - `AnthropicProvider::new()` [~263] - still accepts `temperature: Option<f32>` for backward compat but does NOT send it
  - `fn temperature(&self) -> f32` [~878] - returns `self.temperature` (preserves trait contract for callers like g3-planner/src/lib.rs:97 that read configured temp to seed OTHER provider requests)
  - `test_request_body_omits_temperature_field` - regression test guarding wire format
- `crates/g3-providers/src/openai.rs:84-87` - OpenAI also strips temperature ('seem to fail') — likely similar issue for o-series models
- `crates/g3-core/src/streaming.rs:301-305` - error log labels temperature as "request-level, may be stripped per-provider"
- **Other providers send temperature normally**: Databricks (line 934), Gemini (line 529), Embedded (line 361)

### ACD Cost Analysis (cache-aware, measured over 94 real sessions)
- `analysis/acd_cost_analysis.md` - full verdict, tables, design, limitations
- `analysis/acd/sweep.py` - cache-aware cost sweep over `.g3/sessions/*/session.json`
- `analysis/acd/results.md` - generated output
- `crates/g3-core/tests/acd_cost_model_test.rs` - 11 property tests for the cost model
- `crates/g3-core/tests/acd_fidelity_characterization_test.rs` - 16 defect regression tests

**Key measured facts:**
- Prefix amplification median **37.3x** (max 139x). Cost = message size x number of subsequent requests containing it. A 55k final prefix can bill 3.9M input tokens.
- Spend breakdown: tool results 39.0%, tool_call inputs 35.0%, system prompt 23.9%, assistant prose 1.4%, user 0.7%.
- Turn-aligned ACD saves **26.4% aggregate**; ceiling is 38.2% (it only fires at user-turn boundaries).
- **ACD is NEGATIVE on single-turn sessions (-0.4% median)**; 24/94 sessions zero-or-negative, worst -4.9%. Long autonomous runs are single-turn = the case ACD helps least.
- **Eager per-tool-call pruning is net-NEGATIVE (-3.7% aggregate), worse than doing nothing in 61/94 sessions.** Each eviction invalidates the cached prefix -> forced 1.25x cache re-write exceeds the 0.1x saving.
- Cache-aligned pruning evicts the SAME set (eviction is idempotent) but defers to the breakpoint slide -> cheaper in 92/94 sessions.
- ACD + cache-aligned pruning together = **35.4% aggregate** (they attack disjoint regions).

**Pricing model**: fresh 1.0x, cache read 0.1x, cache write 1.25x. g3 slides its rolling cache breakpoint every 10 tool calls (`lib.rs` tool loop).

### ACD Fidelity Defects (all fixed)
- `crates/g3-core/src/acd.rs:208` `extract_tool_call_summary()` - now reads BOTH `msg.tool_calls` (structured) and inline JSON, no double-counting. Previously only inline JSON -> **every stub said "no tool calls"** with native providers (the default).
- `crates/g3-providers/src/lib.rs:156` `Message.kind` - changed `#[serde(skip)]` -> `#[serde(default)]`. Previously stubs reloaded as `Regular` on `--resume`, so `rposition(is_dehydrated_stub)` returned None, `dehydrate_start` reset to 0, and already-dehydrated content was re-dehydrated into **nested stubs**.
- `crates/g3-core/src/lib.rs:2009` - `dehydrate_start = (idx + 2).min(history_len)`. The `+2` assumes a Summary always follows the stub, but it's only appended when non-empty; overshoot silently dehydrated nothing.
- `crates/g3-core/src/acd.rs:288` `estimate_fragment_tokens()` - now matches `ContextWindow` (JSON/code len/3, prose len/4, +20/tool call). Was flat len/4 -> >20% undercount on JSON, so `execute_rehydrate()` capacity check would green-light an overflowing rehydration.

### Context Management Mechanism Overlap (avoid double-pruning)
Four overlapping mechanisms exist/proposed:
| mechanism | trigger | action |
|---|---|---|
| `thin_context()` thinnify | every 10% context growth | large tool results in first third -> file refs |
| `thin_context_all()` skinnify | compaction fallback | same, entire context |
| compaction | 80% of window | LLM summarises everything |
| proposed pruning | every 10 tool calls | receipts for stale results, cache-aligned |

Thinnify already does nearly what salience pruning would. The honest implementation is **make thinnify cache-aware + tool-call-cadence**, reusing `resolve_thinned_dir()` / `create_tool_result_modification()`, NOT a fourth mechanism.

**Gotcha**: cost-per-session is a metric that IMPROVES when the agent gets dumber. Pruning cannot be enabled on cost evidence alone - needs re-read-rate metric + task-completion A/B first.