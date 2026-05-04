# Recording Feature Test Procedures

## Overview

Recording is controlled exclusively via the MAVLink camera protocol. Configuration is
set through CLI flags at startup:

- `--recording-path <PATH>` (default: `./recordings`)
- `--min-free-disk-percent <PERCENT>` (default: `10`)

## Prerequisites

- mavlink-camera-manager compiled and running
- A video source (camera, RTSP stream, or fake `ball` source)
- QGroundControl or `pymavlink` for MAVLink commands
- Sufficient disk space for recording tests

## 1. Basic MAVLink Recording Test

### Steps

1. Start mavlink-camera-manager:
   ```bash
   ./mavlink-camera-manager --recording-path /tmp/recordings --min-free-disk-percent 0
   ```
2. Create a stream via REST API (H264 or H265 encoding required)
3. Connect a MAVLink client (QGroundControl, pymavlink, etc.)
4. Wait for camera heartbeat
5. Send `MAV_CMD_VIDEO_START_CAPTURE` (command 2500)
6. Observe `STATUSTEXT` message: `REC START <filename>`
7. Let it record for at least 5 seconds
8. Send `MAV_CMD_VIDEO_STOP_CAPTURE` (command 2501)
9. Observe `STATUSTEXT` message: `REC STOP user_request`
10. Check recording files in `/tmp/recordings/`

### Expected Results

- Recording starts on MAVLink command (ACK = ACCEPTED)
- STATUSTEXT messages are sent on start and stop
- `CAMERA_CAPTURE_STATUS` reports `video_status=1` and increasing `recording_time_ms`
- MP4 segment files appear in the recording directory
- Files are playable with standard video players

### Verification

```bash
ls -la /tmp/recordings/*.mp4
ffprobe /tmp/recordings/<filename>.mp4
```

## 2. Disk Space Threshold Test

### Steps

1. Create a small tmpfs mount:
   ```bash
   sudo mkdir /mnt/small_disk
   sudo mount -t tmpfs -o size=50M tmpfs /mnt/small_disk
   ```
2. Start with a high threshold:
   ```bash
   ./mavlink-camera-manager --recording-path /mnt/small_disk --min-free-disk-percent 95
   ```
3. Attempt to start recording via MAVLink

### Expected Results

- Recording start returns `MAV_RESULT_DENIED`
- `STATUSTEXT` with severity WARNING reports the disk space error

### Cleanup

```bash
sudo umount /mnt/small_disk
sudo rmdir /mnt/small_disk
```

## 3. Crash Recovery Test

### Steps

1. Start recording
2. Let it record for at least 5 seconds
3. Forcefully kill the process:
   ```bash
   kill -9 $(pgrep mavlink-camera)
   ```
4. Check the recording directory

### Expected Results

- Multiple 1-second MP4 segments exist
- All completed segments are playable
- At most 1 second of video is lost (the segment being written at kill time)

## 4. Automated Test Script

```bash
python -m venv tests/.venv
source tests/.venv/bin/activate
pip install -r tests/requirements.txt

cargo build
python tests/test_mavlink_recording.py
python tests/test_mavlink_recording.py --duration 10 --verbose
```

## MAVLink Commands Reference

| Command | ID | Description |
|---|---|---|
| `MAV_CMD_VIDEO_START_CAPTURE` | 2500 | Start recording (param2 = status frequency Hz) |
| `MAV_CMD_VIDEO_STOP_CAPTURE` | 2501 | Stop recording |
| `MAV_CMD_REQUEST_CAMERA_CAPTURE_STATUS` | 527 | Get recording status |

## STATUSTEXT Messages

| Event | Severity | Format |
|---|---|---|
| Recording started | INFO | `REC START <filename>` |
| Recording stopped | INFO | `REC STOP <reason>` |
| Recording error | WARNING | `REC ERROR <reason>` |

## Troubleshooting

### Recording doesn't start

1. Check logs for errors
2. Verify disk space is above `--min-free-disk-percent` threshold
3. Ensure stream is active and producing video
4. Check that video encoding is H264 or H265

### Recording files are corrupt

1. Check GStreamer installation:
   ```bash
   gst-inspect-1.0 splitmuxsink
   gst-inspect-1.0 mp4mux
   gst-inspect-1.0 h264parse
   ```

### MAVLink commands not working

1. Verify MAVLink connection is established (camera heartbeat received)
2. Check system/component ID configuration
3. Ensure the stream has MAVLink enabled (`disable_mavlink: false`)
