// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Comment-preserving config migrations (ADR-036 Slice 8).
//!
//! These migrations operate on a [`toml_edit::DocumentMut`] so that
//! untouched parts of the user's config (comments, whitespace, key
//! ordering) survive the rewrite. They back the
//! `conductorctl migrate-config --routing` CLI path.

pub mod raw_to_route;

pub use raw_to_route::{MigrationReport, migrate_raw_to_routes};
