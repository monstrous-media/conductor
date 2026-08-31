// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Migrate legacy `Trigger::Raw` + `Action::MidiForward` mappings into
//! top-level `[[routes]]` entries (ADR-036 D2 / Slice 8).
//!
//! Operates on a [`toml_edit::DocumentMut`] so that comments and
//! formatting in the rest of the file survive the rewrite. Emits
//! post-mapping routes (ADR-036 Phase 3 removed the `phase` field).
//!
//! Lowering semantics (must match Slice 3):
//! - For each `[[modes.mappings]]` whose `trigger.type == "Raw"`:
//!   - `action.type == "MidiForward"` → emit a `[[routes]]` entry with
//!     `from` = the Raw `device` (or `"*"`), `to` = the MidiForward
//!     `target`, `modes = [<mode name>]`,
//!     `enabled = true`, a migration `description`, an optional `filter`
//!     (from the Raw `channel` / `message_types`), and an optional
//!     `transform` (the MidiForward `transform`, wrapped as a
//!     `SignalTransform::Midi` — i.e. `type = "Midi"` is injected).
//!     The mapping is then removed from its mode.
//!   - any other action → ABORT the whole migration with an error naming
//!     the mode and the offending mapping.

use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, Value, value};

/// Summary of what a migration pass did.
#[derive(Debug, Default, Clone)]
pub struct MigrationReport {
    /// Human-readable description of each rewrite performed.
    pub rewrites: Vec<String>,
    /// Non-fatal problems encountered (best-effort migrations).
    pub errors: Vec<String>,
}

const MIGRATION_DESCRIPTION: &str = "Migrated from Raw trigger (ADR-036)";

/// Forward migration: `Trigger::Raw` + `MidiForward` → `[[routes]]`.
///
/// Returns `Err(msg)` if any `Raw` trigger is paired with a non-`MidiForward`
/// action (mirrors the blocking Slice 3 error). On success the document is
/// mutated in place and a [`MigrationReport`] is returned.
pub fn migrate_raw_to_routes(doc: &mut DocumentMut) -> Result<MigrationReport, String> {
    let mut report = MigrationReport::default();
    // Collected (route_table, leading_comment) pairs to append after the walk.
    let mut new_routes: Vec<Table> = Vec::new();

    let Some(modes_item) = doc.get_mut("modes") else {
        return Ok(report);
    };
    let Some(modes) = modes_item.as_array_of_tables_mut() else {
        return Ok(report);
    };

    for mode_idx in 0..modes.len() {
        // Take the mode name first (immutable borrow scope).
        let mode_name = {
            let mode = modes.get(mode_idx).expect("index in range by construction");
            mode.get("name")
                .and_then(Item::as_str)
                .unwrap_or("")
                .to_string()
        };

        let mode = modes
            .get_mut(mode_idx)
            .expect("index in range by construction");

        let Some(mappings_item) = mode.get_mut("mappings") else {
            continue;
        };
        let Some(mappings) = mappings_item.as_array_of_tables_mut() else {
            continue;
        };

        // Build a replacement ArrayOfTables, keeping non-Raw mappings.
        let mut retained = ArrayOfTables::new();
        let original = std::mem::replace(mappings, ArrayOfTables::new());

        for mapping in original.into_iter() {
            match build_route_from_mapping(&mapping, &mode_name) {
                MappingClass::NotRaw => retained.push(mapping),
                MappingClass::Abort(msg) => return Err(msg),
                MappingClass::Route { route, from, to } => {
                    report
                        .rewrites
                        .push(format!("mode '{mode_name}': Raw → route '{from}' → '{to}'"));
                    new_routes.push(route);
                }
            }
        }

        *mappings = retained;
    }

    // Append generated routes to the top-level [[routes]] array.
    if !new_routes.is_empty() {
        let routes = ensure_routes_array(doc);
        for route in new_routes {
            routes.push(route);
        }
    }

    Ok(report)
}

// ────────────────────────────────────────────────────────────────
// Forward-migration helpers
// ────────────────────────────────────────────────────────────────

enum MappingClass {
    /// Not a Raw trigger — keep the mapping untouched.
    NotRaw,
    /// Raw + MidiForward — emit this route table and drop the mapping.
    Route {
        route: Table,
        from: String,
        to: String,
    },
    /// Raw + non-MidiForward — abort with this message.
    Abort(String),
}

/// Classify a `[[modes.mappings]]` table and, when it is a Raw +
/// MidiForward mapping, build the equivalent route table.
fn build_route_from_mapping(mapping: &Table, mode_name: &str) -> MappingClass {
    let Some(trigger) = mapping.get("trigger").and_then(Item::as_table_like) else {
        return MappingClass::NotRaw;
    };
    let trigger_type = trigger.get("type").and_then(Item::as_str).unwrap_or("");
    if trigger_type != "Raw" {
        return MappingClass::NotRaw;
    }

    let action = mapping.get("action").and_then(Item::as_table_like);
    let action_type = action
        .and_then(|a| a.get("type"))
        .and_then(Item::as_str)
        .unwrap_or("");
    if action_type != "MidiForward" {
        return MappingClass::Abort(format!(
            "Trigger::Raw in mode '{mode_name}' is paired with a non-MidiForward action \
             (action.type = '{action_type}'). Raw is a MIDI routing primitive (ADR-036 D2) \
             and only supports the MidiForward action; pair it with MidiForward or replace \
             it with a specific trigger."
        ));
    }
    let action = action.expect("checked above");

    // from = Raw.device or "*"
    let from = trigger
        .get("device")
        .and_then(Item::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| "*".to_string());
    // to = MidiForward.target
    let to = action
        .get("target")
        .and_then(Item::as_str)
        .unwrap_or("")
        .to_string();

    let mut route = Table::new();
    route.set_implicit(false);
    route["from"] = value(from.clone());
    route["to"] = value(to.clone());

    let mut modes_arr = Array::new();
    modes_arr.push(mode_name);
    route["modes"] = value(modes_arr);

    // filter (omit if Raw had neither channel nor message_types)
    if let Some(filter) = build_filter_inline(trigger) {
        route["filter"] = value(filter);
    }

    // transform (wrap MidiForward.transform as SignalTransform::Midi)
    if let Some(transform) = build_transform_inline(action) {
        route["transform"] = value(transform);
    }

    route["enabled"] = value(true);
    route["description"] = value(MIGRATION_DESCRIPTION);

    // Best-effort: carry the migrated mapping's leading comment/whitespace
    // decor onto the new route so comments aren't silently dropped.
    if let Some(prefix) = mapping.decor().prefix()
        && prefix
            .as_str()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
    {
        route.decor_mut().set_prefix(prefix.clone());
    }

    MappingClass::Route { route, from, to }
}

/// Build the inline `filter` table from a Raw trigger's `channel` /
/// `message_types`. Returns `None` when neither is present (matches
/// `build_filter_from_raw` in conductor-core).
fn build_filter_inline(trigger: &dyn toml_edit::TableLike) -> Option<Value> {
    let channel = trigger.get("channel").and_then(Item::as_integer);
    let message_types = trigger
        .get("message_types")
        .and_then(Item::as_array)
        .filter(|a| !a.is_empty());

    if channel.is_none() && message_types.is_none() {
        return None;
    }

    let mut filter = toml_edit::InlineTable::new();
    if let Some(types) = message_types {
        filter.insert("message_types", Value::Array(types.clone()));
    }
    if let Some(ch) = channel {
        let mut channels = Array::new();
        channels.push(ch);
        filter.insert("channels", Value::Array(channels));
    }
    Some(Value::InlineTable(filter))
}

/// Build the inline route `transform` from a MidiForward action's
/// `transform` field, injecting `type = "Midi"` to form a
/// `SignalTransform::Midi` value. Returns `None` if absent.
fn build_transform_inline(action: &dyn toml_edit::TableLike) -> Option<Value> {
    let inner = action.get("transform")?.as_table_like()?;

    let mut transform = toml_edit::InlineTable::new();
    transform.insert("type", Value::from("Midi"));
    for (k, v) in inner.iter() {
        if let Some(val) = v.as_value() {
            transform.insert(k, val.clone());
        }
    }
    Some(Value::InlineTable(transform))
}

/// Get (creating if needed) the top-level `[[routes]]` array of tables.
fn ensure_routes_array(doc: &mut DocumentMut) -> &mut ArrayOfTables {
    if !doc.contains_key("routes") || !doc["routes"].is_array_of_tables() {
        doc["routes"] = Item::ArrayOfTables(ArrayOfTables::new());
    }
    doc["routes"]
        .as_array_of_tables_mut()
        .expect("just ensured array of tables")
}
