// ADR-034 §D6 — Provenance type serde tests.

use conductor_core::config::{ConfigRevision, Initiator, PeerIdentity, Provenance, Source};

#[test]
fn provenance_roundtrips_through_json() {
    let prov = Provenance {
        initiator: Initiator::Llm {
            provider: "anthropic".to_string(),
            model: "claude-opus-4-7".to_string(),
            plan_id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
        },
        source: Source::InMemoryEdit,
        peer: None,
    };
    let json = serde_json::to_string(&prov).expect("serialise");
    let back: Provenance = serde_json::from_str(&json).expect("deserialise");
    assert_eq!(back, prov);
}

#[test]
fn source_disk_import_carries_path_and_revision() {
    let src = Source::DiskImport {
        path: "/home/user/.config/conductor/user.toml".into(),
        revision: ConfigRevision::from_bytes([0xab; 32]),
    };
    let json = serde_json::to_string(&src).expect("serialise");
    assert!(json.contains("disk_import"));
    let back: Source = serde_json::from_str(&json).expect("deserialise");
    assert_eq!(back, src);
}

#[test]
fn source_rollback_forced_requires_reason() {
    let src = Source::RollbackForced {
        reason: "downgrade after schema_version mismatch".to_string(),
    };
    let json = serde_json::to_string(&src).expect("serialise");
    let back: Source = serde_json::from_str(&json).expect("deserialise");
    assert_eq!(back, src);
}

#[test]
fn peer_identity_macos_serialises_with_platform_tag() {
    let peer = PeerIdentity::Macos {
        team_id: "ABC123XYZ4".to_string(),
        bundle_id: Some("media.monstrous.conductor".to_string()),
        pid: 4242,
    };
    let json = serde_json::to_string(&peer).expect("serialise");
    assert!(json.contains("\"platform\":\"macos\""));
    let back: PeerIdentity = serde_json::from_str(&json).expect("deserialise");
    assert_eq!(back, peer);
}

#[test]
fn peer_identity_linux_includes_exe_path_for_audit_only() {
    // exe_path is informational — these tests just verify it round-trips.
    // Authorization decisions must NEVER consult this field.
    let peer = PeerIdentity::Linux {
        uid: 1000,
        exe_path: "/usr/local/bin/conductorctl".into(),
        pid: 7777,
    };
    let json = serde_json::to_string(&peer).expect("serialise");
    assert!(json.contains("\"platform\":\"linux\""));
    let back: PeerIdentity = serde_json::from_str(&json).expect("deserialise");
    assert_eq!(back, peer);
}

#[test]
fn peer_identity_windows_serialises_with_platform_tag() {
    // Windows is a reserved variant (Conductor does not ship on Windows today),
    // but its wire format must still be pinned so a serde tag or field rename
    // can't regress unnoticed behind the macOS/Linux coverage.
    let peer = PeerIdentity::Windows {
        sid: "S-1-5-21-3623811015-3361044348-30300820-1013".to_string(),
        pid: 4242,
    };
    let json = serde_json::to_string(&peer).expect("serialise");
    assert!(
        json.contains("\"platform\":\"windows\""),
        "Windows peer identity must carry the snake_case platform tag, got: {json}"
    );
    assert!(json.contains("\"sid\":\"S-1-5-21-3623811015-3361044348-30300820-1013\""));
    assert!(json.contains("\"pid\":4242"));
    let back: PeerIdentity = serde_json::from_str(&json).expect("deserialise");
    assert_eq!(back, peer);
}

/// Compile-time exhaustiveness guard for `PeerIdentity`.
///
/// The runtime coverage test below iterates a hand-maintained list of
/// variants, which on its own could silently fall behind the enum. This
/// `match` has no wildcard arm, so adding a new `PeerIdentity` variant makes
/// it stop compiling — forcing whoever adds the variant to also extend the
/// coverage `vec!` below. Keeps the runtime guard honest.
#[allow(dead_code)]
fn assert_peer_identity_exhaustive(p: &PeerIdentity) {
    match p {
        PeerIdentity::Macos { .. } | PeerIdentity::Linux { .. } | PeerIdentity::Windows { .. } => {}
    }
}

#[test]
fn every_peer_identity_variant_distinguishes_via_platform_tag() {
    // Mirrors the Initiator/Source kind-tag coverage: every PeerIdentity variant
    // must serialise to a distinct `platform` tag. The list below is kept in
    // lock-step with the enum by `assert_peer_identity_exhaustive` (a new
    // variant breaks that match's compilation); this test then asserts each
    // variant's `platform` tag is present and unique.
    let variants = vec![
        PeerIdentity::Macos {
            team_id: "ABC123XYZ4".to_string(),
            bundle_id: None,
            pid: 1,
        },
        PeerIdentity::Linux {
            uid: 1000,
            exe_path: "/x".into(),
            pid: 2,
        },
        PeerIdentity::Windows {
            sid: "S-1-5-18".to_string(),
            pid: 3,
        },
    ];
    let mut platforms = std::collections::HashSet::new();
    for v in variants {
        let j: serde_json::Value = serde_json::to_value(&v).unwrap();
        let platform = j
            .get("platform")
            .and_then(|x| x.as_str())
            .unwrap_or_else(|| panic!("missing platform tag: {v:?}"))
            .to_string();
        assert!(platforms.insert(platform), "duplicate platform tag: {v:?}");
    }
    assert_eq!(platforms.len(), 3);
}

#[test]
fn every_initiator_variant_distinguishes_via_kind_tag() {
    let variants = vec![
        Initiator::Daemon,
        Initiator::Gui,
        Initiator::Cli,
        Initiator::Llm {
            provider: "p".to_string(),
            model: "m".to_string(),
            plan_id: "id".to_string(),
        },
        Initiator::System,
    ];
    let mut kinds = std::collections::HashSet::new();
    for v in variants {
        let j: serde_json::Value = serde_json::to_value(&v).unwrap();
        let kind = j.get("kind").and_then(|x| x.as_str()).unwrap().to_string();
        assert!(kinds.insert(kind), "duplicate kind tag: {v:?}");
    }
    assert_eq!(kinds.len(), 5);
}

#[test]
fn every_source_variant_distinguishes_via_kind_tag() {
    let variants = vec![
        Source::InMemoryEdit,
        Source::DiskImport {
            path: "/x".into(),
            revision: ConfigRevision::from_bytes([0; 32]),
        },
        Source::Rollback,
        Source::RollbackForced {
            reason: "r".to_string(),
        },
        Source::Migration {
            from_schema: 1,
            to_schema: 2,
        },
        Source::DaemonDefault,
        Source::DaemonStartup {
            source_file: "/x".into(),
            revision: ConfigRevision::from_bytes([0; 32]),
        },
        Source::OperatorConfirmation,
    ];
    let mut kinds = std::collections::HashSet::new();
    for v in variants {
        let j: serde_json::Value = serde_json::to_value(&v).unwrap();
        let kind = j.get("kind").and_then(|x| x.as_str()).unwrap().to_string();
        assert!(kinds.insert(kind), "duplicate kind tag: {v:?}");
    }
    assert_eq!(kinds.len(), 8);
}
