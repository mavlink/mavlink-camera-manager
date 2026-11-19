# Video Source Architecture [FULL]

## Overview

Trait-based abstraction for video input sources (Local V4L, ONVIF, Gst, Redirect). Provides camera discovery, configuration, and control interface.

## Architecture

### VideoSource Trait

Core trait defining video source interface:
- `name()`: Get source name
- `source_string()`: Get source identifier string
- `set_control_by_name/id()`: Set camera control values
- `control_value_by_name/id()`: Get camera control values
- `controls()`: Get available controls
- `is_valid()`: Check if source is valid
- `is_shareable()`: Check if source can be shared

### Video Source Types

- **VideoSourceLocal**: Local V4L cameras (Linux only)
- **VideoSourceOnvif**: ONVIF IP cameras
- **VideoSourceGst**: GStreamer-based sources (Fake, QR)
- **VideoSourceRedirect**: Redirect third-party streams

### Camera Discovery

`cameras_available()` function aggregates cameras from all source types:
- Calls each source type's `cameras_available()` implementation
- Returns combined list of `VideoSourceType` enum variants

## Key Patterns

### Trait-Based Polymorphism

Each source type implements `VideoSource` trait, wrapped in `VideoSourceType` enum.

### Camera Controls

Sources can expose camera controls (brightness, saturation, etc.) via control interface. Controls are typed (Bool, Slider, Menu).

### Source Validation

Sources must implement `is_valid()` to check if source is still available/accessible.

## Common Patterns

### Discovering Cameras

```rust
let cameras = cameras_available().await;
// Returns Vec<VideoSourceType> with all available cameras
```

### Getting a Source

```rust
let source = get_video_source(source_string).await?;
// Finds source by source_string identifier
```

### Setting Controls

```rust
set_control(source_string, control_id, value).await?;
// Sets camera control value
```

## Related Modules

- [Stream](stream.md): Stream manager uses video sources
- [Pipeline](pipeline.md): Pipeline type determined by video source type
- [Controls](../src/lib/controls/): Camera control management (ONVIF)

## Common Pitfalls

1. **Source not found**: Always check `is_valid()` before using source
2. **Control errors**: Some controls may not be available on all cameras - handle errors gracefully
3. **Async discovery**: `cameras_available()` is async - await properly
4. **Source sharing**: Check `is_shareable()` if multiple streams need same source

## Decision Rationale

- **Trait-based abstraction**: Allows multiple source implementations with common interface
- **Enum wrapping**: Type-safe source type handling
- **Async discovery**: Camera discovery may involve network/IO operations
- **Control interface**: Unified interface for camera controls across different source types
