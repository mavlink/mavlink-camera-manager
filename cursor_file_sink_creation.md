# File sink creation
_Exported on 1/22/2026 at 20:40:59 GMT-3 from Cursor (2.4.7)_

---

**User**

Your task is to create a file sink

---

**Cursor**


Reviewing the sink architecture and existing implementations to understand the pattern.



Reviewing a complete sink implementation to understand the pattern:



Interviewing to clarify requirements for the file sink.

## Interview: File Sink Requirements

### 1. Goal Discovery
- What should the file sink do?
  - Save video to a file? Which format (MP4, MKV, AVI, etc.)?
  - Continuous recording or segments?
  - Should it record from the start or on-demand?
- What problem does this solve?

### 2. Constraint Gathering
- Any constraints?
  - File size limits?
  - Duration limits?
  - Storage location requirements?
  - Performance considerations?
- Any parts to avoid modifying?
- Dependencies to preserve?

### 3. Approach Discussion
- Use `filesink` or `splitmuxsink` (for segments)?
- Which tee: `video_tee` (raw) or `rtp_tee` (RTP-encoded)?
- File naming: timestamp-based, UUID-based, or configurable?
- Should it support starting/stopping recording dynamically?

### 4. Preference Discovery
- Default file format (MP4, MKV, etc.)?
- Default location (current directory, configurable path)?
- Should it be configurable via REST API?
- Any specific GStreamer pipeline preferences?

### 5. Decision Confirmation
After you answer, I’ll summarize the approach for your approval before implementation.

Please answer these questions so I can proceed.

---

**User**

@Untitled-1 (1-4) 

---

**Cursor**


Reviewing relevant modules to understand the architecture, then confirming understanding before implementation.



Checking where video encoding happens in the pipeline:



## Summary of Understanding

1. Goal: File sink for MP4 recording, on-demand start/stop, via MAVLink and REST API.
2. Constraints:
   - Monitor disk space; estimate file size per hour
   - Configurable threshold (percentage/size/hours)
   - Stop before threshold or remove oldest recordings (configurable)
   - Clear status feedback and stop reason
3. Approach:
   - Use `filesink` with `mp4mux`
   - Use `video_tee` (encoded H264/H265 before RTP)
   - File naming: timestamp + unique ID + stream name (human-readable)
   - Start/stop by adding/removing sink dynamically (no element reuse)
   - Design for sudden shutdowns (no data loss/corruption)
4. Reliability: Handle sudden power loss/disconnects; ensure no video loss or corruption.

## Clarifications

1. File format: MP4 container with H264/H265. Should we support both, or start with H264?
2. Encoding: Use the encoded stream from `video_tee` (already H264/H265), or re-encode?
3. Sudden shutdown: Use `splitmuxsink` for segmenting, or rely on `mp4mux` with proper EOS handling?
4. Disk space monitoring: Use the existing `sys_info()` function from `mavlink/sys_info.rs`?
5. MAVLink commands: Which commands should trigger start/stop? (e.g., `MAV_CMD_VIDEO_START_STREAMING`, `MAV_CMD_VIDEO_STOP_STREAMING`, or custom commands?)
6. Storage location: Configurable path, or default directory?

Confirm these and I'll proceed. If you prefer defaults, I'll use sensible ones and proceed.

---

**User**

@Untitled-1 (8-14) 

---

**Cursor**

Checking MAVLink camera protocol and CLI structure:

Now I have enough context. Let me present the implementation plan.

---

## Implementation Plan: File Sink for Video Recording

Self-check: ✓ ✓ ✓ ✓

### Overview

This is a significant feature requiring changes across multiple modules. Here's the phased plan:

---

### Phase 1: Core Infrastructure

**1.1 CLI Argument for Recording Path**
- Add `--recording-path` argument (default: CWD ".")
- Add accessor function `recording_path()`

**1.2 FileSink Implementation** (`src/lib/stream/sink/file_sink.rs`)
- New `FileSink` struct implementing `SinkInterface`
- Use `video_tee` (encoded H264/H265 before RTP)
- Pipeline: `queue ! proxysink/proxysrc ! h264parse/h265parse ! splitmuxsink`
- `splitmuxsink` for crash-resistance (creates segments, only last segment lost on crash)
- File naming: `{stream_name}_{timestamp}_{uuid}.mp4`

**1.3 Enum Dispatch Registration**
- Add `File(FileSink)` variant to `Sink` enum
- Add `create_file_sink()` factory function
- Link to `video_tee` (like ImageSink)

---

### Phase 2: Recording Manager

**2.1 Recording State Management** (`src/lib/stream/recording/`)
- `RecordingManager` singleton tracking active recordings per stream
- `RecordingState`: `Idle`, `Recording { start_time, file_path }`, `Stopping`, `Error(reason)`
- `StopReason`: `UserRequest`, `MAVLinkCommand`, `DiskSpaceLow`, `Error(String)`

**2.2 Disk Space Monitoring**
- Use existing `sys_info()` from `mavlink/sys_info.rs`
- Configurable threshold (percentage, size in MB, or hours of recording left)
- Behavior options: `StopRecording` or `RemoveOldestAndContinue`
- Background task checking disk space periodically

**2.3 Recording Files Management**
- Track recorded files with metadata (path, size, start_time, duration)
- API to list recordings, delete recordings
- Estimate file size per hour based on bitrate

---

### Phase 3: MAVLink Integration

**3.1 Update Camera Capabilities**
```rust
flags: CameraCapFlags::CAMERA_CAP_FLAGS_HAS_VIDEO_STREAM 
     | CameraCapFlags::CAMERA_CAP_FLAGS_CAPTURE_VIDEO
```

**3.2 Handle Recording Commands**
- `MAV_CMD_VIDEO_START_CAPTURE` → Start recording
- `MAV_CMD_VIDEO_STOP_CAPTURE` → Stop recording

**3.3 Update Capture Status**
- `MAV_CMD_REQUEST_CAMERA_CAPTURE_STATUS` → Return actual `video_status` (0=idle, 1=running) and `recording_time_ms`

---

### Phase 4: REST API

**4.1 Recording Endpoints**
- `POST /streams/{id}/recording/start` - Start recording (optional: path override)
- `POST /streams/{id}/recording/stop` - Stop recording
- `GET /streams/{id}/recording/status` - Get recording status
- `GET /recordings` - List all recordings
- `DELETE /recordings/{filename}` - Delete a recording

**4.2 Configuration Endpoints**
- `GET /recording/config` - Get recording configuration
- `PUT /recording/config` - Update threshold/behavior settings

---

### Phase 5: Configuration & Settings

**5.1 Settings Schema**
```rust
struct RecordingSettings {
    default_path: String,
    disk_threshold: DiskThreshold,  // Percentage(u8) | SizeMB(u64) | HoursLeft(f32)
    low_space_behavior: LowSpaceBehavior,  // StopRecording | RemoveOldest
}
```

**5.2 Persistence**
- Add to settings file for persistence across restarts

---

### Technical Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Muxer | `splitmuxsink` | Most crash-resistant; only loses last segment on power failure |
| Tee | `video_tee` | Contains encoded H264/H265 before RTP packetization |
| File format | MP4 (`.mp4`) | Widely compatible, H264/H265 friendly |
| Segments | 10-second fragments | Balance between crash-resistance and file count |
| Encoding | Passthrough | Never re-encode; use stream as-is |

---

### Risks & Mitigations

1. **Sudden shutdown**: Using `splitmuxsink` with short fragments minimizes data loss
2. **Disk full**: Monitoring with configurable threshold, automatic stop or cleanup
3. **Multiple streams**: Each stream has independent recording state
4. **Format compatibility**: H264/H265 only initially; design allows future expansion

---

### Questions Before Proceeding

1. **Segment duration**: 10 seconds reasonable, or prefer different value?
2. **Auto-start**: Should recording auto-start with stream? (I suggest: no, only on-demand)
3. **Max recordings**: Should we limit total number of recording files?
4. **REST API priority**: Implement MAVLink first, then REST, or both together?

Confirm this plan and I'll begin implementation with Phase 1.

---

**User**

@Untitled-1 (22-28) 

---

**Cursor**

Updated plan confirmed. Starting implementation with MAVLink integration first.

Adding CLI argument for recording path:

Let me check the full UdpSink again and understand the pipeline structure better:

Creating the FileSink implementation:

Updating the sink module to register FileSink:

Now I need to update the pipeline module to route FileSink to `video_tee`:

Checking if chrono is already a dependency:

Creating the RecordingManager module:

Fixing the compilation errors:

Let me update the recording manager to use `sysinfo` crate directly instead of the private module:

Updating CAMERA_CAP_FLAGS to include CAPTURE_VIDEO:

Let me add recording support methods to the stream manager:

Now let me add the MAVLink command handlers:

Adding recording REST API endpoints to pages.rs:

## Summary - File Sink Implementation

The core file sink feature has been implemented. Here's what was done:

### Phase 1: Core Infrastructure ✓
- **CLI argument**: `--recording-path <PATH>` (default: ".")
- **FileSink**: New sink type using `splitmuxsink` with 1-second segments for crash-resistance
- **Sink Registration**: Added `File(FileSink)` variant, `create_file_sink()` factory, routes to `video_tee`

### Phase 2: Recording Manager ✓
- **RecordingManager**: Singleton tracking active recordings per stream
- **Types**: `RecordingState`, `StopReason`, `RecordingConfig`, `DiskThreshold`, `LowSpaceBehavior`, `RecordingLimit`
- **Disk Space Monitoring**: Configurable thresholds (percentage, MB, hours left)

### Phase 3: MAVLink Integration ✓
- **CAMERA_CAP_FLAGS**: Added `CAPTURE_VIDEO` flag
- **MAV_CMD_VIDEO_START_CAPTURE**: Start recording via MAVLink
- **MAV_CMD_VIDEO_STOP_CAPTURE**: Stop recording via MAVLink
- **CAMERA_CAPTURE_STATUS**: Returns actual `video_status` and `recording_time_ms`

### Phase 4: REST API ✓
- `POST /recording/start?stream_name=<name>` - Start recording
- `POST /recording/stop?stream_name=<name>` - Stop recording
- `GET /recording/status?stream_name=<name>` - Get recording status
- `GET /recording/list` - List all active recordings
- `GET /recording/config` - Get recording configuration
- `PUT /recording/config` - Update recording configuration

### Phase 5: Settings (Pending)
Remaining work for full feature completion:
- Persist `RecordingConfig` in settings file
- Implement `record_when` auto-start logic (on stream start)
- Implement `auto_restart_on_error` logic
- Implement recording file limits enforcement

### Files Created/Modified:
- `src/lib/cli/manager.rs` - Added `--recording-path` argument
- `src/lib/stream/sink/file_sink.rs` - **NEW** - FileSink implementation
- `src/lib/stream/sink/mod.rs` - Registered FileSink
- `src/lib/stream/pipeline/mod.rs` - Route FileSink to video_tee
- `src/lib/stream/recording/mod.rs` - **NEW** - Recording module
- `src/lib/stream/recording/types.rs` - **NEW** - Recording types
- `src/lib/stream/recording/manager.rs` - **NEW** - Recording manager
- `src/lib/stream/mod.rs` - Added recording module
- `src/lib/stream/manager.rs` - Added recording start/stop methods
- `src/lib/mavlink/mavlink_camera.rs` - Added recording MAVLink handlers
- `src/lib/server/pages.rs` - Added recording REST endpoints
- `src/lib/server/manager.rs` - Registered recording routes

Should I continue with Phase 5 (settings persistence and auto-start logic)?

---

**User**

Your task is to create a file sink

@Untitled-1 (1-4) 

@Untitled-1 (8-14) 

@Untitled-1 (22-28) 

yes

---

**Cursor**

Checking the settings module to understand how to persist recording configuration:

Adding recording configuration to settings:

Adding more CLI arguments for recording:

## Implementation Complete - File Sink Feature

All phases have been successfully implemented. Here's the complete summary:

### Files Created

| File | Purpose |
|------|---------|
| `src/lib/stream/sink/file_sink.rs` | FileSink implementation using `splitmuxsink` |
| `src/lib/stream/recording/mod.rs` | Recording module entry point |
| `src/lib/stream/recording/types.rs` | Recording types (RecordingState, RecordingConfig, etc.) |
| `src/lib/stream/recording/manager.rs` | Recording manager singleton |

### Files Modified

| File | Changes |
|------|---------|
| `src/lib/cli/manager.rs` | Added `--recording-path`, `--recording-auto-start`, `--recording-auto-restart` |
| `src/lib/stream/sink/mod.rs` | Registered FileSink, added `create_file_sink()` |
| `src/lib/stream/pipeline/mod.rs` | Route FileSink to `video_tee` |
| `src/lib/stream/mod.rs` | Added recording module, auto-start logic |
| `src/lib/stream/manager.rs` | Added `start_recording`, `stop_recording` methods |
| `src/lib/mavlink/mavlink_camera.rs` | Added `CAPTURE_VIDEO` flag, recording MAVLink handlers |
| `src/lib/server/pages.rs` | Added recording REST endpoints |
| `src/lib/server/manager.rs` | Registered recording routes |
| `src/lib/settings/manager.rs` | Added recording config persistence |
| `src/main.rs` | Added recording manager initialization |

### Features Implemented

1. **FileSink** - MP4 recording with 1-second segments via `splitmuxsink`
2. **CLI Arguments**:
   - `--recording-path <PATH>` (default: ".")
   - `--recording-auto-start` - Auto-start when stream starts
   - `--recording-auto-restart` - Auto-restart on error
3. **MAVLink Integration**:
   - `CAMERA_CAP_FLAGS_CAPTURE_VIDEO` advertised
   - `MAV_CMD_VIDEO_START_CAPTURE` / `MAV_CMD_VIDEO_STOP_CAPTURE`
   - `CAMERA_CAPTURE_STATUS` returns actual `video_status` and `recording_time_ms`
4. **REST API Endpoints**:
   - `POST /recording/start?stream_name=<name>`
   - `POST /recording/stop?stream_name=<name>`
   - `GET /recording/status?stream_name=<name>`
   - `GET /recording/list`
   - `GET /recording/config`, `PUT /recording/config`
5. **Recording Configuration** (persisted in settings):
   - `record_when`: `never` | `on_stream_start`
   - `auto_restart_on_error`: bool
   - `disk_threshold`: percentage | size_mb | hours_left | none
   - `low_space_behavior`: stop_recording | remove_oldest
   - `limit`: none | max_files | max_duration | max_size
6. **Disk Space Monitoring** - Checks threshold before starting

### Technical Details

- Uses `video_tee` (encoded H264/H265 before RTP)
- `splitmuxsink` with 1-second fragments for crash-resistance
- File naming: `{stream_name}_{timestamp}_{segment}.mp4`
- Supports H264 and H265 (no re-encoding)

---

**User**

right, and how are you gonna test it?

---

**Cursor**

Good question. Testing a GStreamer-based recording feature involves several layers. Let me ask about your testing expectations:

Let me check how existing tests are structured in this codebase:

---

**User**

Implement the plan as specified, it is attached for your reference. Do NOT edit the plan file itself.

To-do's from the plan have already been created. Do not create them again. Mark them as in_progress as you work, starting with the first one. Don't stop until you have completed all the to-dos.

---

**Cursor**

I'll implement the testing plan. Let me start by reading the relevant files to understand the current state.

Let me check the existing todos and update them as I start working.

Starting with the first task - adding unit tests to `file_sink.rs`. I'll add tests for `RecordingState` default/transitions and encoding validation.

Now let me add unit tests to the recording manager.

Now let me add integration tests to file_sink.rs using the test helpers.

Now let me verify that all tests compile correctly.

I need to make the `test` module public so tests can access it.

Let me fix the warning about the useless comparison.

Now let me run the tests.

There seems to be an issue with argument parsing. Let me run the tests with the correct format.

The CLI argument parser is consuming test harness arguments. Let me check how this is handled in the project.

The CLI argument parser in this project consumes test harness arguments. This is an existing issue in the project. Let me run the tests without any filter pattern to verify the tests work.

I need to fix the failing tests. The issues are:
1. Unicode characters in Rust's `is_alphanumeric()` includes Japanese characters
2. Some tests need to be async with Tokio runtime

All 55 tests pass. Let me check for lint errors and update the todos.

All tasks are complete. Here's a summary of what was implemented:

## Summary of Changes

### 1. Unit Tests for `file_sink.rs`
- `test_sanitize_filename` - original test preserved
- `test_sanitize_filename_edge_cases` - edge cases including empty strings, special chars, unicode
- `test_recording_state_default` - verifies `RecordingState::Idle` is the default
- `test_recording_state_variants` - tests all state variants
- `test_stop_reason_variants` - tests all stop reason variants

### 2. Integration Tests for `file_sink.rs`
- `test_file_sink_rejects_unsupported_encoding` - verifies YUYV encoding is rejected
- `test_file_sink_creation_h264` - verifies FileSink creates correctly for H264
- `test_file_sink_get_sdp_returns_error` - verifies SDP is not available for file sink
- `test_file_sink_recording_state` - verifies recording state after creation
- `test_file_sink_recording_duration` - verifies duration tracking

### 3. Unit Tests for `recording/types.rs`
- Serialization/deserialization tests for all types: `RecordingState`, `StopReason`, `RecordWhen`, `DiskThreshold`, `LowSpaceBehavior`, `RecordingLimit`, `RecordingConfig`, `RecordingStatus`, `RecordedFile`
- Default value tests for all types

### 4. Unit Tests for `recording/manager.rs`
- `test_sanitize_filename` - filename sanitization
- `test_get_disk_space_returns_values` - disk space query
- `test_get_disk_space_invalid_path` - fallback behavior
- `test_check_disk_space_*` - all threshold types (percentage, size_mb, hours_left, none)
- Async tests for manager functions

### 5. Test Helpers in `custom/test.rs`
- `create_fake_h264_stream_info()` - creates H264 test stream info
- `create_fake_h265_stream_info()` - creates H265 test stream info
- `temp_recording_path()` - creates temp directory for tests
- `cleanup_recording_path()` - cleans up temp directories
- `create_unsupported_encoding_stream_info()` - creates stream with unsupported encoding for error testing

### 6. Manual Test Documentation
Created `docs/testing/recording.md` with procedures for:
- Real camera recording tests
- Disk space threshold tests
- Crash recovery tests
- MAVLink command tests
- Auto-start recording tests
- Recording limits tests
- REST API endpoint tests

All 55 tests pass successfully.

