//! # mcm-api
//!
//! Standalone API contract crate for **Mavlink Camera Manager** (MCM).
//!
//! This crate defines every type that crosses the MCM HTTP / WebSocket boundary
//! (REST request/response bodies, signalling protocol messages, pipeline stats, etc.)
//! and is the single source of truth for the TypeScript bindings shipped to the frontend.
//!
//! ## Version policy
//!
//! `mcm-api` follows [Semantic Versioning](https://semver.org/).
//!
//! While the crate version is below `1.0.0`, the API is considered **unstable** and
//! breaking changes may occur in **minor** version bumps (`0.x.0`). Patch releases
//! (`0.x.y`) are always backward-compatible.
//!
//! Once `1.0.0` is released the usual semver rules apply:
//!
//! | Change kind | Version bump |
//! |---|---|
//! | Removing or renaming a public type, field, or enum variant | **major** |
//! | Changing a serde representation (field names, tag format) | **major** |
//! | Adding a new required (non-`Option`) field to a struct | **major** |
//! | Adding a new public type or module | minor |
//! | Adding a new `Option` field (with `#[serde(default)]`) | minor |
//! | Adding a new enum variant (when `#[non_exhaustive]`) | minor |
//! | Bug fixes, doc changes, internal refactors | patch |

pub mod v1;
