# Mavlink Camera Manager
[![Build status](https://travis-ci.org/patrickelectric/mavlink-camera-manager.svg)](https://travis-ci.org/patrickelectric/mavlink-camera-manager)
[![Cargo download](https://img.shields.io/crates/d/mavlink-camera-manager)](https://crates.io/crates/mavlink-camera-manager)
[![Crate info](https://img.shields.io/crates/v/mavlink-camera-manager.svg)](https://crates.io/crates/mavlink-camera-manager)
[![Documentation](https://docs.rs/mavlink-camera-manager/badge.svg)](https://docs.rs/mavlink-camera-manager)

The Mavlink Camera Manager is an extensible cross-platform camera server.

It provides a RTSP service for sharing video stream and a MAVLink camera protocol compatible API to configure ground control stations (E.g: QGroundControl).

# How to test it

## Get video via player
You can get the video via VLC or any other media player that can receive video via rtsp

### Guide:
1. Start `mavlink-camera-manager` via `cargo run` or calling the binary directly.
2. Open player and set the rtsp string that the command line provides
3. Done

## Get video via Ground Control Stations
The video should automatically popup if you are using any modern GCS, like QGroundControl, that has support for MAVLink camera messages.

### Guide:
1. Start `mavlink-camera-manager` via `cargo run` or calling the binary directly.
2. Start `mavproxy` or `sim_vehicle` with `--out=udpbcast:0.0.0.0:14550`
   - By default mavlink-camera-manager uses 14550 to perform the mavlink connection
3. Open your Ground Control Station
4. Done
