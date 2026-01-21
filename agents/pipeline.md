# Pipeline Architecture [FULL]

## Overview

GStreamer pipeline implementations for different video source types (V4L, ONVIF, Fake, QR, Redirect). All pipelines share common `PipelineState` and use enum dispatch for polymorphic behavior.

## Architecture

### Pipeline Types

- **V4lPipeline**: Local V4L camera pipelines (Linux only)
- **OnvifPipeline**: ONVIF IP camera pipelines
- **FakePipeline**: Fake/test pipelines
- **QrPipeline**: QR code generation pipelines
- **RedirectPipeline**: Redirect third-party streams

### Common PipelineState

All pipeline types share `PipelineState` containing:
- `pipeline`: GStreamer pipeline instance
- `video_tee`: Tee element for Image/Zenoh sinks (raw video)
- `rtp_tee`: Tee element for UDP/RTSP/WebRTC sinks (RTP encoded)
- `sinks`: HashMap of active sinks
- `runner`: PipelineRunner for async pipeline management

### Stream Splitting

- **video_tee**: Used for Image and Zenoh sinks (need raw video frames)
- **rtp_tee**: Used for UDP, RTSP, and WebRTC sinks (need RTP-encoded streams)

## Key Patterns

### Enum Dispatch

```rust
#[enum_dispatch(PipelineGstreamerInterface)]
pub enum Pipeline {
    V4l(V4lPipeline),
    Fake(FakePipeline),
    // ...
}
```

**Rationale**: Zero-cost abstraction with compile-time dispatch over enum variants.

### Trait Interface

`PipelineGstreamerInterface` trait defines common interface:
- `is_running()`: Check if pipeline is active

### Pipeline Creation

`Pipeline::try_new()` creates appropriate pipeline type based on `VideoSourceType`.

## Common Patterns

### Adding a Sink

Sinks link to appropriate tee (video_tee or rtp_tee) via request pads. Manager handles sink addition/removal.

### Pipeline State Access

Use `inner_state_mut()` or `inner_state_as_ref()` to access shared `PipelineState`.

## Related Modules

- [Stream](stream.md): Stream manager that uses pipelines
- [Sink](sink.md): Sinks that connect to pipeline tees
- [Video](video.md): Video source types that determine pipeline type

## Common Pitfalls

1. **Wrong tee selection**: Use video_tee for Image/Zenoh, rtp_tee for UDP/RTSP/WebRTC
2. **Pipeline state access**: Always use provided accessor methods, don't access state directly
3. **GStreamer thread safety**: GStreamer operations must happen in main thread context

## Decision Rationale

- **Enum dispatch**: Compile-time polymorphism without runtime overhead
- **Shared PipelineState**: Common structure reduces duplication, ensures consistency
- **Tee-based splitting**: Allows multiple sinks from single source without duplicating encoding
