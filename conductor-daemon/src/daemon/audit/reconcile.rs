// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-034 §D8.3 — startup reconciliation of the audit outbox.
//!
//! On daemon startup the outbox (`audit-outbox.log`) may contain rows from a
//! mutation that was in flight when the previous process died. This module
//! classifies each mutation (grouped by id) against the revision of the
//! `live.toml` that was actually loaded, so the caller can:
//!
//! - **`Completed`** (`Pending` + `Applied`) / **`RolledBack`** (`Pending` +
//!   `Failed`): a resolved mutation — flush it to SQLite (sub-slice B2).
//! - **`PromoteToApplied`** (`Pending` only, and `live.toml`'s revision matches
//!   the row's `intended_revision`): the publish completed but the `Applied`
//!   marker write was lost. Append an `Applied` marker, then flush.
//! - **`PendingAtCrash`** (`Pending` only, revision does NOT match): a mutation
//!   was in flight and did NOT publish — surface to the operator as an
//!   [`AuditEventType::ConfigMutationPendingAtCrash`] audit event (emitted at
//!   startup via `AuditLogger::log_pending_at_crash_batch`).
//!
//! [`AuditEventType::ConfigMutationPendingAtCrash`]:
//!     crate::daemon::audit::AuditEventType::ConfigMutationPendingAtCrash
//!
//! This module is pure: it takes the already-recovered, already-chain-verified
//! [`OutboxEntry`] slice (from [`crate::daemon::audit::AuditOutbox::open`]) plus
//! the loaded revision string, and returns the classification. It performs no
//! I/O and reads no clock.

use std::collections::BTreeMap;

use super::outbox::{OutboxEntry, OutboxPhase};

/// What startup should do with one mutation's outbox rows (§D8.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationDisposition {
    /// `Pending` + `Applied` present: the mutation completed durably. Flush to
    /// SQLite.
    Completed,
    /// `Pending` + `Failed` present: the mutation was rolled back after the
    /// pending enqueue. Flush to SQLite.
    RolledBack,
    /// `Pending` only, and `live.toml`'s revision == the row's
    /// `intended_revision`: the publish completed but the `Applied`-marker write
    /// was lost. The caller appends an `Applied` marker, then flushes.
    PromoteToApplied,
    /// `Pending` only, and the revision does NOT match (or the pending row
    /// carried no `intended_revision`): the mutation was in flight at crash and
    /// did not publish. Surfaced at startup as a
    /// `ConfigMutationPendingAtCrash` audit event.
    PendingAtCrash,
    /// The id's rows don't match the one-`Pending`-then-one-marker invariant —
    /// an orphan marker (no `Pending`), a marker before its `Pending`, duplicate
    /// `Pending` rows, or duplicate/contradictory markers. The mutate path
    /// cannot produce any of these (fresh-UUID ids, in-order append, hash-chain
    /// verification), so it means the outbox is semantically corrupted. Surfaced
    /// (never collapsed to a guessed terminal state) so the operator sees the
    /// integrity violation.
    Inconsistent,
}

/// One mutation's reconciliation result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconciledMutation {
    /// The mutation id shared by the `Pending` row and its marker.
    pub id: String,
    /// What to do with it.
    pub disposition: MutationDisposition,
    /// The intended revision recorded on the `Pending` row, if any.
    pub intended_revision: Option<String>,
}

/// Reconcile recovered outbox `entries` against the `live_revision` actually
/// loaded at startup (§D8.3). Rows are grouped by mutation id in first-seen
/// (append) order; each group is classified per [`MutationDisposition`].
///
/// **Outbox invariant (guaranteed by the mutate path).** Each mutation id is a
/// fresh UUID that appears as exactly one `Pending` row followed by exactly one
/// resolving marker (`Applied` xor `Failed`) — ids are never reused, rows are
/// appended in that order, and the hash-chain reader rejects any tampered or
/// reordered file (fatal `Corruption`) *before* this runs. The only legitimate
/// deviation is a `Pending` with no marker (the marker write was lost to a tail
/// truncation, §D8.1).
///
/// This function therefore treats **exactly** these row-shapes as well-formed
/// and classifies **anything else** as [`MutationDisposition::Inconsistent`]
/// rather than guessing — surfacing the integrity violation to the operator:
///
/// | rows for one id | disposition |
/// |---|---|
/// | 1×`Pending`, 1×`Applied` (in order) | `Completed` |
/// | 1×`Pending`, 1×`Failed` (in order) | `RolledBack` |
/// | 1×`Pending`, no marker | `PromoteToApplied` (revision matches) / `PendingAtCrash` |
/// | anything else — orphan marker, marker-before-`Pending`, duplicate `Pending`, duplicate or contradictory markers | `Inconsistent` |
pub fn reconcile(entries: &[OutboxEntry], live_revision: &str) -> Vec<ReconciledMutation> {
    struct Group {
        order: usize,
        intended_revision: Option<String>,
        pending_count: usize,
        applied_count: usize,
        failed_count: usize,
        /// A resolving marker was seen while `pending_count == 0` — i.e. before
        /// any `Pending` for this id (an orphan / out-of-order marker).
        marker_before_pending: bool,
    }
    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    let mut next_order = 0usize;

    for entry in entries {
        let rec = &entry.record;
        // #2380: a ChainReset record is a meta-event (operator reset the chain
        // via `audit resume`), not a config mutation — never group/reconcile it.
        if matches!(rec.phase, OutboxPhase::ChainReset) {
            continue;
        }
        let g = groups.entry(rec.id.clone()).or_insert_with(|| {
            let order = next_order;
            next_order += 1;
            Group {
                order,
                intended_revision: None,
                pending_count: 0,
                applied_count: 0,
                failed_count: 0,
                marker_before_pending: false,
            }
        });
        match rec.phase {
            OutboxPhase::Pending => {
                g.pending_count += 1;
                // Never overwrite a known intended revision with `None` (a torn
                // re-enqueue carrying no revision must not erase the real one).
                if rec.intended_revision.is_some() {
                    g.intended_revision = rec.intended_revision.clone();
                }
            }
            OutboxPhase::Applied => {
                if g.pending_count == 0 {
                    g.marker_before_pending = true;
                }
                g.applied_count += 1;
            }
            OutboxPhase::Failed => {
                if g.pending_count == 0 {
                    g.marker_before_pending = true;
                }
                g.failed_count += 1;
            }
            // Skipped above (meta-event, not a mutation); arm kept for
            // exhaustiveness.
            OutboxPhase::ChainReset => {}
        }
    }

    let mut out: Vec<(usize, ReconciledMutation)> = groups
        .into_iter()
        .map(|(id, g)| {
            let total_markers = g.applied_count + g.failed_count;
            // Well-formed iff exactly one Pending, at most one marker, and any
            // marker came AFTER the Pending. Otherwise the rows are corrupt.
            let well_formed =
                g.pending_count == 1 && total_markers <= 1 && !g.marker_before_pending;
            let disposition = if !well_formed {
                MutationDisposition::Inconsistent
            } else if g.applied_count == 1 {
                MutationDisposition::Completed
            } else if g.failed_count == 1 {
                MutationDisposition::RolledBack
            } else {
                // Pending only — was the publish actually committed? Compare
                // `live.toml`'s loaded revision against the intended one.
                match g.intended_revision.as_deref() {
                    Some(rev) if rev == live_revision => MutationDisposition::PromoteToApplied,
                    _ => MutationDisposition::PendingAtCrash,
                }
            };
            (
                g.order,
                ReconciledMutation {
                    id,
                    disposition,
                    intended_revision: g.intended_revision,
                },
            )
        })
        .collect();

    // Preserve append order for deterministic, operator-friendly output.
    out.sort_by_key(|(order, _)| *order);
    out.into_iter().map(|(_, m)| m).collect()
}

/// Convenience: the reconciled mutations that need an operator's attention —
/// those that were in flight at crash and did not publish (`PendingAtCrash`).
/// Returns references into `reconciled` (each carries its `id` +
/// `intended_revision`).
pub fn pending_at_crash(reconciled: &[ReconciledMutation]) -> Vec<&ReconciledMutation> {
    reconciled
        .iter()
        .filter(|m| m.disposition == MutationDisposition::PendingAtCrash)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::audit::AuditOutbox;

    /// Build an outbox file in a temp dir from a sequence of `(closure)`
    /// appends, then return the recovered entries. Each append `fdatasync`s
    /// before returning (the outbox write recipe), so the file is fully durable
    /// by the time `read_outbox_entries` runs — no flush race.
    fn entries_from(appends: impl FnOnce(&mut AuditOutbox)) -> Vec<OutboxEntry> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit-outbox.log");
        let (mut ob, _) = AuditOutbox::open(path.clone()).unwrap();
        appends(&mut ob);
        crate::daemon::audit::read_outbox_entries(&path).unwrap()
    }

    #[test]
    fn pending_plus_applied_is_completed() {
        let entries = entries_from(|ob| {
            ob.enqueue_pending("m1", None, Some("rev1".into()), 1)
                .unwrap();
            ob.mark_applied("m1", 2).unwrap();
        });
        let r = reconcile(&entries, "rev1");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].id, "m1");
        assert_eq!(r[0].disposition, MutationDisposition::Completed);
    }

    #[test]
    fn pending_plus_failed_is_rolled_back() {
        let entries = entries_from(|ob| {
            ob.enqueue_pending("m1", None, Some("rev1".into()), 1)
                .unwrap();
            ob.mark_failed("m1", 2).unwrap();
        });
        let r = reconcile(&entries, "whatever");
        assert_eq!(r[0].disposition, MutationDisposition::RolledBack);
    }

    #[test]
    fn pending_only_matching_live_is_promote() {
        // Publish completed (live.toml hash == intended) but the Applied marker
        // write was lost.
        let entries = entries_from(|ob| {
            ob.enqueue_pending("m1", None, Some("rev-final".into()), 1)
                .unwrap();
        });
        let r = reconcile(&entries, "rev-final");
        assert_eq!(r[0].disposition, MutationDisposition::PromoteToApplied);
        assert_eq!(r[0].intended_revision.as_deref(), Some("rev-final"));
    }

    #[test]
    fn pending_only_not_matching_live_is_pending_at_crash() {
        let entries = entries_from(|ob| {
            ob.enqueue_pending("m1", None, Some("rev-intended".into()), 1)
                .unwrap();
        });
        // live.toml loaded a DIFFERENT revision → the mutation never published.
        let r = reconcile(&entries, "rev-on-disk");
        assert_eq!(r[0].disposition, MutationDisposition::PendingAtCrash);
        assert_eq!(pending_at_crash(&r).len(), 1);
    }

    #[test]
    fn pending_only_without_intended_revision_is_pending_at_crash() {
        let entries = entries_from(|ob| {
            ob.enqueue_pending("m1", None, None, 1).unwrap();
        });
        let r = reconcile(&entries, "anything");
        assert_eq!(r[0].disposition, MutationDisposition::PendingAtCrash);
    }

    #[test]
    fn multiple_mutations_classified_in_append_order() {
        let entries = entries_from(|ob| {
            // m1 completed
            ob.enqueue_pending("m1", None, Some("r1".into()), 1)
                .unwrap();
            ob.mark_applied("m1", 2).unwrap();
            // m2 in flight at crash (intended != live)
            ob.enqueue_pending("m2", None, Some("r2".into()), 3)
                .unwrap();
            // m3 publish-completed-marker-lost (intended == live)
            ob.enqueue_pending("m3", None, Some("r-live".into()), 4)
                .unwrap();
        });
        let r = reconcile(&entries, "r-live");
        assert_eq!(
            r.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["m1", "m2", "m3"],
            "append order preserved"
        );
        assert_eq!(r[0].disposition, MutationDisposition::Completed);
        assert_eq!(r[1].disposition, MutationDisposition::PendingAtCrash);
        assert_eq!(r[2].disposition, MutationDisposition::PromoteToApplied);
        assert_eq!(pending_at_crash(&r).len(), 1);
    }

    /// Contradictory markers (both Applied and Failed for one id) → Inconsistent,
    /// surfaced rather than silently collapsed to whichever appeared last.
    #[test]
    fn both_markers_is_inconsistent_regardless_of_order() {
        let a_then_f = entries_from(|ob| {
            ob.enqueue_pending("m1", None, Some("r1".into()), 1)
                .unwrap();
            ob.mark_applied("m1", 2).unwrap();
            ob.mark_failed("m1", 3).unwrap();
        });
        assert_eq!(
            reconcile(&a_then_f, "r1")[0].disposition,
            MutationDisposition::Inconsistent
        );

        let f_then_a = entries_from(|ob| {
            ob.enqueue_pending("m1", None, Some("r1".into()), 1)
                .unwrap();
            ob.mark_failed("m1", 2).unwrap();
            ob.mark_applied("m1", 3).unwrap();
        });
        assert_eq!(
            reconcile(&f_then_a, "r1")[0].disposition,
            MutationDisposition::Inconsistent,
            "order-independent — contradictory markers always Inconsistent"
        );
    }

    /// Orphan markers (no preceding Pending) are impossible in a well-formed
    /// outbox → Inconsistent, never a guessed terminal state.
    #[test]
    fn orphan_marker_is_inconsistent() {
        let applied = entries_from(|ob| {
            ob.mark_applied("orphan", 1).unwrap();
        });
        assert_eq!(
            reconcile(&applied, "x")[0].disposition,
            MutationDisposition::Inconsistent
        );
        let failed = entries_from(|ob| {
            ob.mark_failed("orphan", 1).unwrap();
        });
        assert_eq!(
            reconcile(&failed, "x")[0].disposition,
            MutationDisposition::Inconsistent
        );
    }

    /// A marker appearing BEFORE its `Pending` (out of order) is Inconsistent.
    #[test]
    fn marker_before_pending_is_inconsistent() {
        let entries = entries_from(|ob| {
            ob.mark_applied("m1", 1).unwrap();
            ob.enqueue_pending("m1", None, Some("r1".into()), 2)
                .unwrap();
        });
        assert_eq!(
            reconcile(&entries, "r1")[0].disposition,
            MutationDisposition::Inconsistent
        );
    }

    /// Duplicate Pending rows, or a duplicate terminal marker, are Inconsistent.
    #[test]
    fn duplicate_rows_are_inconsistent() {
        let dup_pending = entries_from(|ob| {
            ob.enqueue_pending("m1", None, Some("r1".into()), 1)
                .unwrap();
            ob.enqueue_pending("m1", None, Some("r2".into()), 2)
                .unwrap();
        });
        assert_eq!(
            reconcile(&dup_pending, "r2")[0].disposition,
            MutationDisposition::Inconsistent
        );

        let dup_applied = entries_from(|ob| {
            ob.enqueue_pending("m1", None, Some("r1".into()), 1)
                .unwrap();
            ob.mark_applied("m1", 2).unwrap();
            ob.mark_applied("m1", 3).unwrap();
        });
        assert_eq!(
            reconcile(&dup_applied, "r1")[0].disposition,
            MutationDisposition::Inconsistent
        );
    }

    #[test]
    fn empty_outbox_reconciles_to_nothing() {
        assert!(reconcile(&[], "rev").is_empty());
        assert!(pending_at_crash(&[]).is_empty());
    }
}
