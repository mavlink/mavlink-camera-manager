# mcm-api

Standalone API contract crate for [Mavlink Camera Manager](https://github.com/bluerobotics/mavlink-camera-manager).

This crate defines every public type that crosses the HTTP / WebSocket boundary
(REST request/response bodies, WebRTC signalling messages, pipeline statistics,
etc.) and is the single source of truth for the TypeScript bindings shipped to
the frontend.

## Version policy

See the crate-level documentation (`api/src/lib.rs`) for the full semver policy.

In short: while the version is below `1.0.0`, breaking changes may occur in
minor releases. After `1.0.0`, standard semver rules apply.
