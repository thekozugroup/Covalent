use covalent_protocol::{
    ExportedDeviceSettings, Manifest, PROTOCOL_VERSION, PairingInvitation, RelativePath,
    SETTINGS_SCHEMA_VERSION,
};
use proptest::prelude::*;

#[test]
fn versioned_json_fixtures_match_rust_contracts() {
    let settings: ExportedDeviceSettings =
        serde_json::from_str(include_str!("../../../fixtures/contracts/settings-v1.json"))
            .expect("settings fixture");
    settings.validate().expect("settings validation");
    assert_eq!(settings.schema_version, SETTINGS_SCHEMA_VERSION);

    let invitation: PairingInvitation = serde_json::from_str(include_str!(
        "../../../fixtures/contracts/pairing-invitation-v1.json"
    ))
    .expect("pairing fixture");
    assert_eq!(invitation.protocol_version, PROTOCOL_VERSION);

    let manifest: Manifest =
        serde_json::from_str(include_str!("../../../fixtures/contracts/manifest-v1.json"))
            .expect("manifest fixture");
    assert_eq!(manifest.protocol_version, PROTOCOL_VERSION);
    assert_eq!(manifest.entries.len(), 1);
}

#[test]
fn adversarial_restore_path_fixture_is_rejected() {
    for value in include_str!("../../../fixtures/security/invalid-restore-paths.txt").lines() {
        assert!(RelativePath::new(value).is_err(), "accepted {value:?}");
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 512,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn accepted_relative_paths_always_satisfy_confinement_invariants(value in ".{0,5000}") {
        if let Ok(path) = RelativePath::new(value.clone()) {
            prop_assert!(!path.as_str().is_empty());
            prop_assert!(path.as_str().len() <= 4_096);
            prop_assert!(!path.as_str().starts_with('/'));
            prop_assert!(!path.as_str().contains(['\\', '\0']));
            for component in path.components() {
                prop_assert!(!component.is_empty());
                prop_assert!(component != "." && component != "..");
                prop_assert!(component.len() <= 255);
            }
        }
    }
}
