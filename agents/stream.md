# Stream Management Architecture [FULL]

## Overview

The stream management system is the core of mavlink-camera-manager, orchestrating video pipelines, sinks, and stream lifecycle. It uses a singleton manager pattern with thread-safe access via `Arc<RwLock<Manager>>`.

## Architecture

### Manager Pattern

- **Singleton**: `lazy_static!` with `Arc<RwLock<Manager>>` for global state
- **Location**: `src/lib/stream/manager.rs`
- **Rationale**: Thread-safe global state management with read-heavy operations (RwLock allows concurrent reads)

### Stream Lifecycle

- **Stream Struct**: Contains `StreamState`, pipeline ID, video/stream information, error tracking, and watcher handle
- **Watcher Task**: Monitors pipeline health, auto-recreates failed pipelines (self-healing)
- **Error Tracking**: `Arc<RwLock<Result<()>>>` for error state shared across threads
- **Termination**: `Arc<RwLock<bool>>` for graceful shutdown

### Key Patterns

1. **Singleton Manager**: `MANAGER` static provides global access
2. **Stream State**: `StreamState` contains pipeline, MAVLink camera, video source info
3. **Auto-Recovery**: Watcher task automatically recreates failed pipelines
4. **Error Propagation**: Errors stored in `Arc<RwLock<Result<()>>>` for thread-safe access

## Common Patterns

### Creating a Stream

```rust
// Stream creation handled by Manager::create_stream()
// Automatically creates pipeline, starts watcher, tracks state
```

### Adding Sinks

Sinks are dynamically added to pipelines via the manager. Each sink links to pipeline tees.

### Error Handling

Errors are captured in `Arc<RwLock<Result<()>>>` and can be queried via stream error state.

## Related Modules

- [Pipeline](pipeline.md): Pipeline implementations used by streams
- [Sink](sink.md): Sink implementations added to streams
- [Video](video.md): Video source abstractions
- [MAVLink](mavlink.md): MAVLink integration for stream communication

## Common Pitfalls

1. **Not checking error state**: Always check stream error state before assuming pipeline is healthy
2. **Blocking operations**: Avoid blocking operations in watcher tasks - use async/await
3. **State synchronization**: Be careful with `Arc<RwLock<>>` - ensure proper locking order to avoid deadlocks

## Decision Rationale

- **Singleton pattern**: Needed for global stream management across the application
- **Watcher tasks**: Auto-recovery ensures streams stay alive even if pipelines fail
- **Arc<RwLock<>>**: Thread-safe shared state with read-heavy access pattern
