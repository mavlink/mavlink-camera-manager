# Settings/Configuration Architecture [FULL]

## Overview

Persistent configuration management using singleton pattern. Stores stream configurations, camera settings, and application state that persists across restarts.

## Architecture

### Manager Pattern

- **Singleton**: `lazy_static!` with `Arc<RwLock<Manager>>` for global state
- **Location**: `src/lib/settings/manager.rs`
- **Rationale**: Thread-safe global state management for configuration

### Persistent Storage

Settings are persisted to disk (location configurable). On startup, settings are loaded and streams are restored.

### Configuration Types

- **Stream Configurations**: Video source, encoding, endpoints, etc.
- **Camera Settings**: Control values, camera-specific settings
- **Application State**: Global application configuration

## Key Patterns

### Singleton Access

Settings manager accessed via `MANAGER` static. Provides thread-safe read/write access to configuration.

### Persistence

Settings automatically saved when modified. Loaded on application startup.

### Stream Restoration

On startup, settings manager can restore previously configured streams based on saved configuration.

## Common Patterns

### Reading Settings

```rust
let settings = settings::manager::get_streams();
// Get stream configurations
```

### Updating Settings

```rust
settings::manager::set_streams(streams);
// Update and persist stream configurations
```

## Related Modules

- [Stream](stream.md): Stream manager uses settings for stream restoration
- [Server](server.md): REST API exposes settings management

## Common Pitfalls

1. **Race conditions**: Use proper locking when reading/writing settings
2. **Persistence failures**: Handle disk I/O errors gracefully
3. **Configuration validation**: Validate settings before persisting

## Decision Rationale

- **Singleton pattern**: Global configuration state needed across application
- **Persistent storage**: Settings must survive application restarts
- **Thread-safe access**: `Arc<RwLock<>>` ensures safe concurrent access
