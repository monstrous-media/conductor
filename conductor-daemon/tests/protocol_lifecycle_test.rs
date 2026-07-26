// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! ADR-039 §4.2 — protocol lifecycle coverage matrix + enforcement (#1761).
//!
//! The matrix below is the **structured source of truth** for "which lifecycle
//! stage is implemented for which protocol" (ADR-039 §D1). Per the R2 revision
//! the flow is inverted: instead of a Rust test parsing a brittle Markdown
//! table, the typed matrix here is authoritative and
//! `docs/cross-protocol-parity/lifecycle-coverage.md` is **generated** from it.
//!
//! Enforcement (acceptance criteria):
//!   * Every `Done`/`Baseline` cell names a Rust symbol that **must resolve** —
//!     the [`done!`] / [`baseline!`] macros reference the type via `PhantomData`,
//!     so if the implementation is removed or renamed this test file fails to
//!     **compile** and CI goes red. The path string is `stringify!`d from the
//!     same token, so the cell and its proof can't drift.
//!   * Every non-`Done` cell must be a known sub-ADR id (`039-A/B/C`) or an
//!     `NotApplicable` with a non-empty reason — asserted at runtime.
//!   * The committed generated Markdown must match what the matrix produces
//!     (run with `LIFECYCLE_REGEN=1` to regenerate after a matrix change).
//!
//! Lives in `conductor-daemon` (not `core`) because the `InputSource` impls it
//! proves (`MidiInputSource` / `HidInputSource`) are daemon-resident (R4).

use conductor_core::config::protocol::Protocol;
use std::fmt::Write as _;

/// The six lifecycle stages every protocol is evaluated against (ADR-039 §D1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    InputListener,
    TypedTriggers,
    CatchAll,
    ForwardAction,
    OutputConnector,
    CrossProtocolTransform,
}

impl Stage {
    const ALL: [Stage; 6] = [
        Stage::InputListener,
        Stage::TypedTriggers,
        Stage::CatchAll,
        Stage::ForwardAction,
        Stage::OutputConnector,
        Stage::CrossProtocolTransform,
    ];

    /// 1-based row label for the generated table.
    fn label(self) -> &'static str {
        match self {
            Stage::InputListener => "1. Input Listener",
            Stage::TypedTriggers => "2. Typed Triggers",
            Stage::CatchAll => "3. Catch-All (route)",
            Stage::ForwardAction => "4. Forward Action",
            Stage::OutputConnector => "5. Output Connector",
            Stage::CrossProtocolTransform => "6. Cross-Protocol Transform",
        }
    }
}

/// Coverage state for one (protocol, stage) cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Coverage {
    /// Implemented; the `&str` is the backing Rust symbol path (compile-proven).
    Done(&'static str),
    /// MIDI cross-protocol transforms are the baseline the others mirror; the
    /// `&str` is the backing symbol path (compile-proven).
    Baseline(&'static str),
    /// Deferred to a sub-ADR (`039-A` / `039-B` / `039-C`).
    SubAdr(&'static str),
    /// Intentionally unsupported; the `&str` is the documented reason.
    NotApplicable(&'static str),
}

/// Known sub-ADR ids — a `SubAdr` cell must name one of these.
const KNOWN_SUB_ADRS: &[&str] = &["039-A", "039-B", "039-C"];

/// `Done(stringify!(Type))` that also compile-proves `Type` resolves: if the
/// implementation is removed/renamed, `PhantomData::<Type>` fails to compile.
macro_rules! done {
    ($t:ty) => {{
        let _ = core::marker::PhantomData::<$t>;
        Coverage::Done(stringify!($t))
    }};
}

/// Like [`done!`] but for the MIDI cross-protocol baseline cell.
macro_rules! baseline {
    ($t:ty) => {{
        let _ = core::marker::PhantomData::<$t>;
        Coverage::Baseline(stringify!($t))
    }};
}

/// The authoritative lifecycle matrix (ADR-039 §4.2). One entry per
/// (protocol, stage); built in a fn (not `const`) so the `done!`/`baseline!`
/// compile-proofs can reference the backing types.
fn lifecycle() -> Vec<(Protocol, Stage, Coverage)> {
    use Protocol::{ArtNet, Hid, Midi, Osc};
    use Stage::*;
    vec![
        // ── MIDI: first-class across the lifecycle ──
        (
            Midi,
            InputListener,
            done!(conductor_daemon::midi_device::MidiInputSource),
        ),
        (
            Midi,
            TypedTriggers,
            done!(conductor_core::config::types::Trigger),
        ),
        (
            Midi,
            CatchAll,
            done!(conductor_daemon::route_engine::RouteEngine),
        ),
        (Midi, ForwardAction, done!(conductor_core::actions::Action)),
        (
            Midi,
            OutputConnector,
            done!(conductor_core::midi_output::MidiOutputManager),
        ),
        (
            Midi,
            CrossProtocolTransform,
            baseline!(conductor_core::transform::MidiTransform),
        ),
        // ── HID: input-class; output dropped (ADR D7); forwarding is 039-B ──
        (
            Hid,
            InputListener,
            done!(conductor_daemon::gamepad_device::HidInputSource),
        ),
        (
            Hid,
            TypedTriggers,
            done!(conductor_core::config::types::Trigger),
        ),
        // ADR-039-B #1762 (step 4d): HID is now first-class across the input
        // lifecycle. Catch-all HID routes evaluate through the same
        // `RouteEngine` as MIDI (steps 1-3); the `HidForward` action shipped in
        // step 4b; the `HidToMidi`/`HidToOsc`/`HidToArtNet` cross-protocol
        // transforms are `SignalTransform` variants. The live cutover (step 4c)
        // feeds a gamepad's events into this path through `HidInputSource`.
        (
            Hid,
            CatchAll,
            done!(conductor_daemon::route_engine::RouteEngine),
        ),
        (Hid, ForwardAction, done!(conductor_core::actions::Action)),
        (
            Hid,
            OutputConnector,
            Coverage::NotApplicable("HID output dropped, ADR D7"),
        ),
        (
            Hid,
            CrossProtocolTransform,
            done!(conductor_core::config::types::SignalTransform),
        ),
        // ── OSC: input listener + catch-all + OscToMidi shipped in ADR-039-A
        //    Slice 1 (#1361); typed triggers (2) shipped in Slice 2 (#2325,
        //    with D17 taint gating); forward action (4) is Slice 3. ──
        (
            Osc,
            InputListener,
            done!(conductor_daemon::osc_parser::ParsedDatagram),
        ),
        (
            Osc,
            TypedTriggers,
            // #2325: OscMessage/OscAddressPattern/OscArgRange — the pattern
            // type is the compile-proof (its module exists only for these).
            done!(conductor_core::osc_pattern::OscPattern),
        ),
        (
            Osc,
            CatchAll,
            done!(conductor_daemon::route_engine::RouteEngine),
        ),
        (
            Osc,
            ForwardAction,
            // #2326: OscForward action — compile-proven on the Action enum.
            done!(conductor_core::actions::Action),
        ),
        (
            Osc,
            OutputConnector,
            done!(conductor_daemon::connector_registry::EndpointRegistry),
        ),
        (
            Osc,
            CrossProtocolTransform,
            done!(conductor_core::config::types::SignalTransform),
        ),
        // ── Art-Net: output-only today; input + triggers are 039-C ──
        (ArtNet, InputListener, Coverage::SubAdr("039-C")),
        (ArtNet, TypedTriggers, Coverage::SubAdr("039-C")),
        (ArtNet, CatchAll, Coverage::SubAdr("039-C")),
        (ArtNet, ForwardAction, Coverage::SubAdr("039-C")),
        (
            ArtNet,
            OutputConnector,
            done!(conductor_daemon::connector_registry::EndpointRegistry),
        ),
        (ArtNet, CrossProtocolTransform, Coverage::SubAdr("039-C")),
    ]
}

/// Column order for the generated table.
const PROTOCOLS: [Protocol; 4] = [
    Protocol::Midi,
    Protocol::Hid,
    Protocol::Osc,
    Protocol::ArtNet,
];

fn cell_display(c: &Coverage) -> String {
    match c {
        Coverage::Done(_) => "Done".to_string(),
        Coverage::Baseline(_) => "baseline".to_string(),
        Coverage::SubAdr(id) => (*id).to_string(),
        Coverage::NotApplicable(reason) => format!("n/a ({reason})"),
    }
}

fn protocol_label(p: Protocol) -> &'static str {
    match p {
        Protocol::Midi => "MIDI",
        Protocol::Hid => "HID",
        Protocol::Osc => "OSC",
        Protocol::ArtNet => "Art-Net",
    }
}

/// Render the matrix to the generated-Markdown artifact.
fn render_markdown(matrix: &[(Protocol, Stage, Coverage)]) -> String {
    let lookup = |p: Protocol, s: Stage| -> &Coverage {
        &matrix
            .iter()
            .find(|(mp, ms, _)| *mp == p && *ms == s)
            .expect("matrix is complete (asserted separately)")
            .2
    };

    let mut out = String::new();
    out.push_str("# ADR-039 Protocol Lifecycle Coverage\n\n");
    out.push_str(
        "<!-- GENERATED from conductor-daemon/tests/protocol_lifecycle_test.rs — \
         do not hand-edit. Run `LIFECYCLE_REGEN=1 cargo test -p conductor-daemon \
         --test protocol_lifecycle_test` to regenerate. -->\n\n",
    );
    out.push_str(
        "Source of truth: the `lifecycle()` matrix (ADR-039 §4.2). Each `Done`/`baseline`\n",
    );
    out.push_str("cell is compile-proven against the Rust symbol listed below; a removed or\n");
    out.push_str("renamed implementation fails the build.\n\n");

    // Coverage table. The header + separator rows are DERIVED from `PROTOCOLS`
    // (not hard-coded) so they cannot silently misalign with the data columns —
    // which iterate the same array — if the column set ever changes (Council
    // review, PR #2245).
    out.push_str("| Stage |");
    for p in PROTOCOLS {
        let _ = write!(out, " {} |", protocol_label(p));
    }
    out.push('\n');
    out.push('|');
    for _ in 0..=PROTOCOLS.len() {
        out.push_str("---|");
    }
    out.push('\n');
    for s in Stage::ALL {
        let _ = write!(out, "| {} ", s.label());
        for p in PROTOCOLS {
            let _ = write!(out, "| {} ", cell_display(lookup(p, s)));
        }
        out.push_str("|\n");
    }

    // Backing-symbol list for Done/Baseline cells.
    out.push_str("\n## Backing symbols (compile-proven)\n\n");
    for (p, s, c) in matrix {
        if let Coverage::Done(path) | Coverage::Baseline(path) = c {
            let _ = writeln!(out, "- {} · {} → `{}`", protocol_label(*p), s.label(), path);
        }
    }
    out
}

// Relative to CARGO_MANIFEST_DIR (conductor-daemon/, one level under repo root).
const GENERATED_DOC: &str = "../docs/cross-protocol-parity/lifecycle-coverage.md";

#[test]
fn matrix_is_complete_one_cell_per_protocol_stage() {
    let matrix = lifecycle();
    assert_eq!(
        matrix.len(),
        PROTOCOLS.len() * Stage::ALL.len(),
        "expected exactly one cell per (protocol, stage) — {} protocols × {} stages",
        PROTOCOLS.len(),
        Stage::ALL.len()
    );
    for p in PROTOCOLS {
        for s in Stage::ALL {
            let n = matrix
                .iter()
                .filter(|(mp, ms, _)| *mp == p && *ms == s)
                .count();
            assert_eq!(
                n, 1,
                "(protocol {p:?}, stage {s:?}) must appear exactly once, found {n}"
            );
        }
    }
}

#[test]
fn non_done_cells_are_known_subadr_or_documented_na() {
    for (p, s, c) in lifecycle() {
        match c {
            // Done/Baseline are compile-proven by the macros — nothing to assert here.
            Coverage::Done(_) | Coverage::Baseline(_) => {}
            Coverage::SubAdr(id) => assert!(
                KNOWN_SUB_ADRS.contains(&id),
                "({p:?}, {s:?}) names unknown sub-ADR {id:?}; expected one of {KNOWN_SUB_ADRS:?}"
            ),
            Coverage::NotApplicable(reason) => assert!(
                !reason.trim().is_empty(),
                "({p:?}, {s:?}) is NotApplicable without a documented reason"
            ),
        }
    }
}

#[test]
fn rendered_header_is_derived_from_protocols() {
    // Council review (PR #2245): the table header must track `PROTOCOLS`, not a
    // hard-coded string, so it can't silently misalign with the data columns if
    // the column set changes. Assert the header row has exactly one column per
    // protocol (plus the leading "Stage" column) and names each protocol.
    let md = render_markdown(&lifecycle());
    let header = md
        .lines()
        .find(|l| l.starts_with("| Stage |"))
        .expect("header row present");
    // A `| a | b |` row split on '|' yields ["", " a ", " b ", ""]; the inner
    // cell count is parts-2.
    let columns = header.split('|').count() - 2;
    assert_eq!(
        columns,
        PROTOCOLS.len() + 1,
        "header must have one column per protocol plus the Stage column: {header}"
    );
    for p in PROTOCOLS {
        assert!(
            header.contains(protocol_label(p)),
            "header must name protocol {p:?}: {header}"
        );
    }
    // The separator row must match the header's column count.
    let sep = md
        .lines()
        .find(|l| l.starts_with("|---|"))
        .expect("separator row present");
    assert_eq!(
        sep.matches("---").count(),
        PROTOCOLS.len() + 1,
        "separator must have one cell per header column: {sep}"
    );
}

#[test]
fn generated_markdown_matches_committed_doc() {
    let matrix = lifecycle();
    // Assert completeness first so render_markdown's lookups can't panic.
    assert_eq!(matrix.len(), PROTOCOLS.len() * Stage::ALL.len());
    let expected = render_markdown(&matrix);
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(GENERATED_DOC);

    if std::env::var_os("LIFECYCLE_REGEN").is_some() {
        std::fs::write(&path, &expected).expect("write generated lifecycle doc");
        return;
    }

    let committed = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read {} ({e}); run `LIFECYCLE_REGEN=1 cargo test -p conductor-daemon \
             --test protocol_lifecycle_test` to generate it",
            path.display()
        )
    });
    assert_eq!(
        committed, expected,
        "lifecycle-coverage.md is stale — regenerate with `LIFECYCLE_REGEN=1 cargo test \
         -p conductor-daemon --test protocol_lifecycle_test` and commit the result"
    );
}
