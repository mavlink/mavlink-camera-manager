# MAVLink Integration Architecture [FULL]

## Overview

MAVLink protocol integration for camera communication. Uses separate OS threads for sender/receiver loops, manages MAVLink camera components, and communicates stream information.

## Architecture

### Threading Model

- **Main Thread**: Tokio async runtime (10 worker threads)
- **MAVLink Sender Thread**: Separate OS thread for sending MAVLink messages
- **MAVLink Receiver Thread**: Separate OS thread for receiving MAVLink messages

**Rationale**: MAVLink communication uses blocking I/O that doesn't fit well in async runtime, so dedicated OS threads are used.

### Manager Pattern

- **Singleton**: `lazy_static!` with `Arc<RwLock<Manager>>` for global state
- **Location**: `src/lib/mavlink/manager.rs`
- **Rationale**: Thread-safe global state management for MAVLink communication

### MAVLink Camera Component

- **MavlinkCamera**: Represents a camera in MAVLink protocol
- **MavlinkCameraComponent**: Camera component management
- **Integration**: Connected to stream manager for stream information

## Key Patterns

### Separate Threads

MAVLink sender/receiver run in dedicated OS threads, not Tokio tasks. This allows blocking I/O without blocking async runtime.

### Stream Communication

MAVLink manager communicates stream information (status, endpoints, etc.) to ground control stations via MAVLink protocol.

### Camera Control

MAVLink can send camera control commands that are handled by the control system.

## Common Patterns

### Initializing MAVLink

Manager initializes sender/receiver threads on startup. Threads run independently of main async runtime.

### Sending Stream Information

When streams are created/updated, MAVLink manager sends appropriate MAVLink messages to advertise stream information.

## Related Modules

- [Stream](stream.md): Stream manager provides stream information to MAVLink
- [Controls](../src/lib/controls/): Camera controls that MAVLink can trigger

## Common Pitfalls

1. **Thread synchronization**: Be careful with shared state between MAVLink threads and main thread
2. **Blocking operations**: MAVLink threads can block - ensure they don't hold locks that main thread needs
3. **Message ordering**: MAVLink messages may arrive out of order - handle appropriately

## Decision Rationale

- **Separate OS threads**: MAVLink I/O is blocking, doesn't fit async model
- **Singleton manager**: Global MAVLink state needed across application
- **Thread-safe access**: `Arc<RwLock<>>` ensures safe concurrent access
