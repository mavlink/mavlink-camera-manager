# User Preferences & Defaults

## Overview

This document tracks user coding preferences, architectural choices, and default decisions for the mavlink-camera-manager project.

## Coding Style

### Error Variable Naming
- **ALWAYS** use `Err(error)`, **NEVER** use `Err(e)`
- This applies to all error handling: `if let Err(error)`, `match` arms, closures, etc.
- Example:
  ```rust
  // CORRECT
  if let Err(error) = some_operation() {
      warn!("Operation failed: {error}");
  }
  
  // WRONG
  if let Err(e) = some_operation() {
      warn!("Operation failed: {e}");
  }
  ```

### Code Organization: Depth-First Top-Down Order

**Algorithm**: Start with the central type. For each field, if it references a custom type defined in this module, define that type immediately after (depth-first), then continue with its fields recursively. After all types, add impl blocks, then functions, then tests.

**Order within a module**:
1. Central type (the primary struct/enum the module is about)
2. Types referenced by central type's fields, depth-first:
   - For field A (e.g., `Vec<TypeA>`), define TypeA
   - For each field in TypeA that uses another custom type, define it immediately
   - Continue recursively until all nested types are defined
   - Then continue with field B of the central type, etc.
3. `impl Default` for central type (if any)
4. `impl` block for central type (methods)
5. Public standalone functions
6. Private helper functions
7. Tests (`#[cfg(test)]`)

**Concrete Example**:
```rust
struct StreamRecordingMetadata {
    sessions: Vec<SessionMetadata>,      // field A
    unified_files: Vec<UnifiedFileMetadata>, // field B
}
struct SessionMetadata {
    chunks: Vec<ChunkMetadata>,          // field A.1
}
struct ChunkMetadata { ... }             // field A.1.1 (leaf)
struct UnifiedFileMetadata {
    validation_info: Option<ValidationInfo>, // field B.1
}
struct ValidationInfo { ... }            // field B.1.1 (leaf)
```

**Traversal order**:
1. `StreamRecordingMetadata` — central type
2. `SessionMetadata` — first field of central type uses this
3. `ChunkMetadata` — SessionMetadata.chunks uses this (go deep first)
4. `UnifiedFileMetadata` — second field of central type (back up, continue)
5. `ValidationInfo` — UnifiedFileMetadata.validation_info uses this
6. `impl Default for StreamRecordingMetadata`
7. `impl StreamRecordingMetadata { ... }`
8. `pub fn list_recorded_streams()`
9. `fn current_timestamp_ms()` (private)
10. `#[cfg(test)] mod tests`

### No Separator Comments
- Do **NOT** add file separator comments like `// Helper functions`, `// Types`, `// Implementations`, etc.
- The code organization should be self-evident from the structure itself

## Architectural Patterns

*(To be populated as preferences are discovered)*

## Default Decisions

*(To be populated as preferences are discovered)*

## Conventions

*(To be populated as preferences are discovered)*

---

**Note**: Preferences are added here as they are discovered during development. These rules are mandatory for all agents working on this codebase.
