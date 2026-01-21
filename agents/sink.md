# Sink Architecture [FULL]

## Overview

Video output sinks (UDP, RTSP, WebRTC, Image, Zenoh) that connect to pipeline tees. Sinks are dynamically added/removed and implement the `SinkInterface` trait via enum dispatch.

## Architecture

### Sink Types

- **UdpSink**: UDP streaming sink
- **RtspSink**: RTSP server sink
- **WebRTCSink**: WebRTC peer connection sink
- **ImageSink**: Image thumbnail/snapshot sink
- **ZenohSink**: Zenoh pub/sub sink

### SinkInterface Trait

All sinks implement `SinkInterface` with methods:
- `link()`: Link sink pad to pipeline tee src pad
- `unlink()`: Unlink sink from pipeline
- `get_id()`: Get sink UUID
- `get_sdp()`: Get SDP description (RFC 8866)
- `start()`: Start the sink
- `eos()`: Send end-of-stream
- `pipeline()`: Get inner pipeline if sink has sub-pipeline

### Enum Dispatch

```rust
#[enum_dispatch(SinkInterface)]
pub enum Sink {
    Udp(UdpSink),
    Rtsp(RtspSink),
    WebRTC(WebRTCSink),
    Image(ImageSink),
    Zenoh(ZenohSink),
}
```

## Key Patterns

### Dynamic Linking

Sinks link to pipeline tees via request pads. This allows dynamic addition/removal without pipeline reconstruction.

**Important**: Read GStreamer docs on [dynamic pipeline manipulation](https://gstreamer.freedesktop.org/documentation/application-development/advanced/pipeline-manipulation.html?gi-language=c#dynamically-changing-the-pipeline).

### Sub-Pipelines

Some sinks (e.g., WebRTC) have their own sub-pipelines. Access via `pipeline()` method.

### Cleanup

Sinks must properly handle EOS and cleanup in `Drop` implementations to prevent resource leaks.

## Common Patterns

### Creating a Sink

Use factory functions:
- `create_udp_sink()`
- `create_rtsp_sink()`
- `create_webrtc_sink()`
- `create_image_sink()`
- `create_zenoh_sink()`

### Linking to Pipeline

Manager handles linking via `SinkInterface::link()`. Sink requests pad from appropriate tee (video_tee or rtp_tee).

## Related Modules

- [Pipeline](pipeline.md): Pipelines that sinks connect to
- [Stream](stream.md): Stream manager that manages sinks

## Common Pitfalls

1. **EOS not sent**: Always call `eos()` before unlinking to ensure proper cleanup
2. **Wrong tee**: Image/Zenoh use video_tee, UDP/RTSP/WebRTC use rtp_tee
3. **Resource leaks**: Ensure `Drop` implementation properly cleans up GStreamer elements
4. **Thread safety**: GStreamer operations must be in main thread context

## Decision Rationale

- **Trait-based polymorphism**: Allows multiple sink implementations with common interface
- **Enum dispatch**: Zero-cost abstraction for sink type dispatch
- **Dynamic linking**: Enables adding/removing sinks without pipeline reconstruction
- **Sub-pipelines**: Some sinks need complex GStreamer pipelines (e.g., WebRTC encoding)
