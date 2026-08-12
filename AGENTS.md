# AGENTS.md

**Last Updated**: 2026-01-23

This document provides comprehensive guidance for AI agents working on the mavlink-camera-manager project. All agents must read the "Must Read" section completely before starting any task.

---

## Must Read [FULL]

**This section is mandatory for ALL agents. Read completely before proceeding.**

### Project Philosophy

1. **Minimize Code Changes**: Preserve existing architecture, only change what's necessary
2. **Cost of Maintainability**: Every abstraction has a cost - justify it. Cost of maintainability and coding is of utmost importance.
3. **Simplicity First**: Architecture should be as simple as possible. Every line of code has impact.
4. **Consistency**: Be as consistent as possible in code and philosophy.

### Coding Style (Critical)

These rules are **mandatory** and violations are costly to fix:

1. **Error variables**: Use `Err(error)`, **NEVER** `Err(e)`. Applies everywhere.
2. **Code order**: Depth-first top-down. Central type first → follow field dependencies recursively → impl blocks → functions → tests. No separator comments.

See [User Preferences](agents/preferences.md) for algorithm and concrete example.

### Code Fragility Policy

- **All code is treated as fragile**, especially behavioral changes
- **Behavioral changes = breaking changes** (unless user explicitly decides otherwise with good reason)
- Many applications depend on current behavior
- **Impact analysis required for all changes**
- Breaking changes must be contained as long as possible, and backward compatibility must be part of the plan

### Agent-User Relationship

- **User is the manager and code owner**
- Agent helps user sustain responsibility over decisions
- Agent must interview user to remove all uncertainties
- Agent guides user, user makes final decisions
- Code owner must be the user, not the agents

### Interview Process

**Simplicity Threshold**: For changes affecting <5 lines or typo fixes, skip interview but still read Must Read section.

**Before making changes (except simple fixes), agent must complete 5 steps:**

1. **Goal Discovery**: What is the specific goal? What problem are we solving? What's the expected outcome?
2. **Constraint Gathering**: Are there constraints (time, compatibility, performance)? Any parts to avoid modifying? Dependencies to preserve?
3. **Approach Discussion**: Present approach, discuss alternatives, identify potential breaking changes, confirm backward compatibility needs
4. **Preference Discovery**: For each decision point, ask user preference. Use defaults from preferences section if user defers.
5. **Decision Confirmation**: Always confirm before proceeding. Summarize approach and get final approval.

**If user says "I don't know"**: Help user explore the problem, suggest possible goals/constraints, ask user to choose or refine.

**If user provides conflicting answers**: Point out the conflict, ask user to clarify which is correct, document the resolution. Do not proceed until conflict is resolved.

**If user says "I changed my mind" mid-interview**: Acknowledge the change, update documented answers from previous steps, continue from current step.

**If user explicitly requests to skip interview for non-simple task**: Still complete Steps 1 (Goal Discovery) and 5 (Decision Confirmation) at minimum. Document that user requested to skip full interview.

**If user unavailable**: Document all assumptions from Steps 1-4, wait for confirmation before proceeding.

### Self-Check Before Proceeding

Agent MUST state self-check completion before proceeding. Format: `Self-check: ✓ ✓ ✓ ✓` (short form preferred) or `Self-check: Must Read=✓, Module Sections=✓, Interview=✓, Fragility=✓` (verbose if clarity needed).

**Check each item:**
1. Have I read the Must Read section? [✓/✗]
2. Have I identified and read all relevant module sections? [✓/✗]
3. Have I interviewed the user and removed all uncertainties? [✓/✗/N/A] (N/A if <5 lines or typo fix)
4. Do I understand the fragility implications of my changes? [✓/✗]

**When to state**: After completing all reading and interview (if required), but before starting implementation.

**If any item is ✗**: Agent must address the failure before proceeding. Only item 3 (interview) can be N/A for simple fixes.

### Examples

**Scenario**: Agent needs to add a new video sink type.

**Good**: Agent reads Must Read, identifies Sink and Pipeline modules, interviews user about sink requirements, confirms approach, states self-check, then implements following existing patterns.

**Bad**: Agent immediately starts coding new sink without reading modules or interviewing user about requirements.

**Impact**: Following the process ensures consistency, avoids breaking changes, and maintains architecture integrity.

---

## Summary / Table of Contents [QUICK]

**Task-Based Navigation Map** - Find your task below and read only the indicated sections:

- **Fixing a bug in video pipeline?** → Read: [Must Read](#must-read), [Pipeline](agents/pipeline.md), [Stream](agents/stream.md)
- **Adding a new sink type?** → Read: [Must Read](#must-read), [Sink](agents/sink.md), [Pipeline](agents/pipeline.md), [User Preferences](agents/preferences.md)
- **Modifying video source?** → Read: [Must Read](#must-read), [Video](agents/video.md), [Stream](agents/stream.md)
- **Changing REST API endpoint?** → Read: [Must Read](#must-read), [Server](agents/server.md)
- **Fixing MAVLink integration?** → Read: [Must Read](#must-read), [MAVLink](agents/mavlink.md), [Stream](agents/stream.md)
- **Updating settings/config?** → Read: [Must Read](#must-read), [Settings](agents/settings.md)
- **Performance optimization?** → Read: [Must Read](#must-read), relevant module sections (identify by: 1) user indicates modules, 2) profiling results, 3) code analysis), [Knowledge](agents/knowledge.md)

**If your task isn't listed**: Read [Must Read](#must-read) + all modules mentioned in task description. If uncertain, read modules that share similar functionality.

**File Reading Guidance**:
- `[QUICK]`: Quick context, high-level patterns - sufficient for understanding only
- `[FULL]`: Required when making changes to that module

---

## Stream Management Architecture [QUICK]

Core streaming system managing video pipelines, sinks, and stream lifecycle. Uses singleton pattern with `Arc<RwLock<Manager>>` for thread-safe global state. Streams are self-healing with watcher tasks that auto-recreate failed pipelines.

**Key Patterns**: Singleton manager, stream lifecycle with watcher, error tracking via `Arc<RwLock<Result<()>>>`, trait-based polymorphism for pipelines and sinks.

**Related**: [Pipeline](agents/pipeline.md), [Sink](agents/sink.md), [Video](agents/video.md), [MAVLink](agents/mavlink.md)

See [Stream Architecture](agents/stream.md) for details.

---

## Pipeline Implementations [QUICK]

GStreamer pipeline implementations (V4L, ONVIF, Fake, QR, Redirect) sharing common `PipelineState`. Each pipeline type creates its GStreamer pipeline with video_tee (for Image/Zenoh sinks) and rtp_tee (for UDP/RTSP/WebRTC sinks).

**Key Patterns**: Enum dispatch via `enum_dispatch`, common PipelineState structure, tee-based stream splitting, trait-based interface (`PipelineGstreamerInterface`).

**Related**: [Stream](agents/stream.md), [Sink](agents/sink.md), [Video](agents/video.md)

See [Pipeline Architecture](agents/pipeline.md) for details.

---

## Sink Implementations [QUICK]

Video output sinks (UDP, RTSP, WebRTC, Image, Zenoh) dynamically added/removed from pipelines. Each sink implements `SinkInterface` trait and links to pipeline tees via request pads. Sinks can have sub-pipelines (e.g., WebRTC).

**Key Patterns**: Trait-based polymorphism (`SinkInterface`), enum dispatch, dynamic sink management, proper EOS and cleanup in `Drop` implementations.

**Related**: [Pipeline](agents/pipeline.md), [Stream](agents/stream.md)

See [Sink Architecture](agents/sink.md) for details.

---

## Video Source Abstractions [QUICK]

Trait-based abstraction for video inputs (Local V4L, ONVIF, Gst, Redirect). `VideoSource` trait defines interface, implementations provide camera discovery and configuration.

**Key Patterns**: Trait-based polymorphism, enum wrapping implementations, camera discovery via `cameras_available()`.

**Related**: [Stream](agents/stream.md), [Pipeline](agents/pipeline.md)

See [Video Source Architecture](agents/video.md) for details.

---

## MAVLink Integration [QUICK]

MAVLink protocol integration with separate sender/receiver threads. Manages MAVLink camera components, communicates stream information, handles camera control commands.

**Key Patterns**: Separate OS threads for sender/receiver loops, MAVLink camera component management, integration with stream manager.

**Related**: [Stream](agents/stream.md)

See [MAVLink Architecture](agents/mavlink.md) for details.

---

## REST API Server [QUICK]

Actix-web REST API server with Swagger UI. Provides endpoints for stream management, camera control, configuration. Uses async/await patterns with Tokio runtime.

**Key Patterns**: Actix-web framework, async request handling, Swagger UI integration, error handling via custom error types.

**Related**: [Stream](agents/stream.md), [Settings](agents/settings.md)

See [Server Architecture](agents/server.md) for details.

---

## Settings/Configuration Management [QUICK]

Persistent configuration management using singleton pattern. Stores stream configurations, camera settings, and application state. Settings persist across restarts.

**Key Patterns**: Singleton manager with `Arc<RwLock<Manager>>`, persistent storage, configuration serialization.

**Related**: [Stream](agents/stream.md), [Server](agents/server.md)

See [Settings Architecture](agents/settings.md) for details.

---

## User Preferences & Defaults [QUICK]

User coding style, architectural preferences, and default decisions. Documented here or in [preferences.md](agents/preferences.md) if exceeds 50 lines.

**If preferences section doesn't exist yet**: Agent should ask user about preferences during interview (Step 4), document them, and create preferences.md if it exceeds 50 lines.

See [User Preferences](agents/preferences.md) for details.

---

## Knowledge & Learnings [QUICK]

Structured knowledge base tracking breaking changes, common bugs, performance optimizations, architecture decisions, pitfalls, and cross-module learnings.

**Update Process**: Agent updates knowledge.md **before committing code or creating pull request**. Include date, context, and links to related modules. Keep entries concise (2-3 lines per item).

**If knowledge.md doesn't exist yet**: Agent should create it with proper structure when first knowledge entry is needed.

See [Knowledge Base](agents/knowledge.md) for details.

---

## Additional Guidelines

- **Compactness Rule**: Main AGENTS.md sections target ~50 lines maximum. If section exceeds 50 lines, extract to `agents/<section>.md` and replace with link + overview (2-3 sentences or bullet points, max 100 words).
- **File Structure**: Start with flat `agents/` directory. Only create subdirectories if category grows beyond 10 files.
- **Version Control**: All AGENTS.md and agents/ files should be in version control. Commit messages: `docs(agents): [description]` for structure changes, `docs(agents): update [section]` for content updates.
- **Maintenance**: Review AGENTS.md structure quarterly (every 3 months from last review) for accuracy, completeness, and relevance.
