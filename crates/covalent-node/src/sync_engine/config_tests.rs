use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use serde_json::{Value, json};
use tempfile::TempDir;
use uuid::Uuid;
use zeroize::Zeroizing;

use super::*;

const OWN_ID: &str = "EA2I6UL-UUWKJDX-NGOC6QS-U2IZR4Z-LXY5CB7-3KBVQRG-4CNFBV7-RACTXQF";
const PEER_ID: &str = "FZ23CS4-PDV743V-32LS44M-TCIUBAQ-RQ3OC4X-XN6EG66-BGUCZH4-OISTMAE";

fn id(value: &str) -> EngineDeviceId {
    EngineDeviceId::parse(value).expect("valid identity")
}

fn key() -> EngineApiKey {
    EngineApiKey::parse(Zeroizing::new("a1".repeat(32))).expect("valid key")
}

fn address(port: u16) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, port))
}

fn socket_path(fixture: &TempDir) -> PathBuf {
    let runtime = fixture.path().join("runtime");
    std::fs::create_dir_all(&runtime).expect("private runtime directory");
    runtime
        .canonicalize()
        .expect("canonical runtime directory")
        .join("engine.sock")
}

fn peer() -> EnginePeerConfig {
    EnginePeerConfig::new(id(PEER_ID), "Peer", address(22001)).expect("peer")
}

fn folder(root: PathBuf, members: Vec<EngineDeviceId>) -> EngineFolderConfig {
    EngineFolderConfig::new(
        Uuid::from_u128(0x12345678123456781234567812345678),
        "Shared folder",
        root,
        members,
    )
    .expect("folder")
}

fn desired(
    fixture: &TempDir,
    peers: Vec<EnginePeerConfig>,
    folders: Vec<EngineFolderConfig>,
    listener: Option<SocketAddr>,
) -> Result<DesiredEngineConfig, EngineConfigError> {
    DesiredEngineConfig::new(
        id(OWN_ID),
        "Local device",
        socket_path(fixture),
        key(),
        listener,
        peers,
        folders,
    )
}

// Shape captured from GET /rest/config on the pinned v2.1.3 worker. Fields
// unrelated to Covalent's renderer are intentionally represented by one
// sentinel to prove expanded defaults are accepted.
fn effective_fixture(config: &DesiredEngineConfig, desired: bool) -> Value {
    let mut devices = vec![json!({
        "deviceID": config.own_id.as_str(),
        "name": config.own_name.as_ref(),
        "addresses": ["dynamic"],
        "compression": "metadata",
        "introducer": false,
        "skipIntroductionRemovals": false,
        "introducedBy": "",
        "autoAcceptFolders": false,
        "untrusted": false,
        "paused": false,
        "numConnections": 1,
        "maxRecvKbps": 0
    })];
    if desired {
        devices.extend(config.peers.iter().map(|peer| {
            json!({
                "deviceID": peer.id.as_str(),
                "name": peer.name.as_ref(),
                "addresses": [format!("tcp://{}", peer.address)],
                "compression": "metadata",
                "introducer": false,
                "skipIntroductionRemovals": false,
                "introducedBy": "",
                "autoAcceptFolders": false,
                "untrusted": false,
                "paused": peer.paused,
                "numConnections": 1,
                "maxRecvKbps": 0
            })
        }));
    }
    let folders = if desired {
        config
            .folders
            .iter()
            .map(|folder| {
                json!({
                    "id": folder.id.to_string(),
                    "label": folder.label.as_ref(),
                    "path": folder.root.to_str().expect("UTF-8 root"),
                    "type": "sendreceive",
                    "filesystemType": "basic",
                    "rescanIntervalS": 3600,
                    "fsWatcherEnabled": true,
                    "fsWatcherDelayS": 10,
                    "fsWatcherTimeoutS": 0,
                    "ignorePerms": true,
                    "autoNormalize": true,
                    "devices": folder.members.iter().map(|member| json!({
                        "deviceID": member.as_str(),
                        "introducedBy": "",
                        "encryptionPassword": ""
                    })).collect::<Vec<_>>(),
                    "versioning": {
                        "type": "simple",
                        "params": {"keep": "100", "cleanoutDays": "0"},
                        "cleanupIntervalS": 3600,
                        "fsPath": "",
                        "fsType": "basic"
                    },
                    "ignoreDelete": false,
                    "maxConflicts": -1,
                    "paused": folder.paused,
                    "markerName": ".stfolder",
                    "disableFsync": false,
                    "copyRangeMethod": "standard",
                    "syncOwnership": false,
                    "sendOwnership": false,
                    "syncXattrs": false,
                    "sendXattrs": false,
                    "blockIndexing": true,
                    "minDiskFree": {"value": 1, "unit": "%"},
                    "caseSensitiveFS": false
                })
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let listener = if desired {
        config
            .listener
            .map(|address| format!("tcp://{address}"))
            .unwrap_or_default()
    } else {
        String::new()
    };
    json!({
        "gui": {
            "enabled": true,
            "useTLS": false,
            "sendBasicAuthPrompt": false,
            "address": config.gui_socket.to_str().expect("UTF-8 socket"),
            "unixSocketPermissions": "0600",
            "apiKey": config.api_key.expose(),
            "metricsWithoutAuth": false,
            "insecureAdminAccess": false,
            "insecureSkipHostcheck": false,
            "insecureAllowFrameLoading": false,
            "theme": "default"
        },
        "options": {
            "listenAddresses": [listener],
            "globalAnnounceServers": [""],
            "globalAnnounceEnabled": false,
            "localAnnounceEnabled": false,
            "relaysEnabled": false,
            "natEnabled": false,
            "startBrowser": false,
            "urAccepted": -1,
            "urSeen": -1,
            "autoUpgradeIntervalH": 0,
            "upgradeToPreReleases": false,
            "crashReportingEnabled": false,
            "announceLANAddresses": false,
            "auditEnabled": false,
            "stunServers": [""],
            "reconnectionIntervalS": 5,
            "maxFolderConcurrency": 1,
            "urURL": "http://127.0.0.1:1/disabled",
            "releasesURL": "http://127.0.0.1:1/disabled",
            "crURL": "http://127.0.0.1:1/disabled"
        },
        "devices": devices,
        "folders": folders,
        "defaults": {"expanded": true}
    })
}

fn assert_effective_mismatch(config: &DesiredEngineConfig, value: &Value) {
    assert_eq!(
        config.verify_effective(value).unwrap_err(),
        EngineConfigError::EffectiveConfigMismatch
    );
}

#[test]
fn validates_exact_canonical_device_ids_and_check_digits() {
    assert_eq!(id(OWN_ID).as_str(), OWN_ID);
    assert_eq!(id(PEER_ID).as_str(), PEER_ID);

    let mut bad_check = OWN_ID.as_bytes().to_vec();
    bad_check[6] = if bad_check[6] == b'A' { b'B' } else { b'A' };
    assert_eq!(
        EngineDeviceId::parse(std::str::from_utf8(&bad_check).expect("ASCII")).unwrap_err(),
        EngineConfigError::InvalidDeviceId
    );
    assert_eq!(
        EngineDeviceId::parse(&OWN_ID.to_ascii_lowercase()).unwrap_err(),
        EngineConfigError::InvalidDeviceId
    );
    assert_eq!(
        EngineDeviceId::parse(&OWN_ID.replace('-', " ")).unwrap_err(),
        EngineConfigError::InvalidDeviceId
    );

    let zero = canonical_id([b'A'; 52]);
    assert_eq!(
        EngineDeviceId::parse(&zero).unwrap_err(),
        EngineConfigError::InvalidDeviceId
    );
    let mut noncanonical_padding = [b'A'; 52];
    noncanonical_padding[0] = b'B';
    noncanonical_padding[51] = b'B';
    assert_eq!(
        EngineDeviceId::parse(&canonical_id(noncanonical_padding)).unwrap_err(),
        EngineConfigError::InvalidDeviceId
    );
}

#[test]
fn derives_canonical_device_identity_from_bounded_certificate_der() {
    let first = EngineDeviceId::from_certificate_der(b"bounded certificate fixture")
        .expect("derived identity");
    let second = EngineDeviceId::from_certificate_der(b"different certificate fixture")
        .expect("derived identity");
    assert_eq!(
        first.as_str(),
        "44NUOLO-OQUSI2S-CBPZFAU-CLHHPYH-A6IJ4ZA-ZQC6JLY-2FSRMLR-3MGNZAF"
    );
    assert_ne!(first, second);
    assert_eq!(
        EngineDeviceId::parse(first.as_str()).expect("round trip"),
        first
    );
    assert_eq!(
        EngineDeviceId::from_certificate_der(&[]).unwrap_err(),
        EngineConfigError::InvalidCertificate
    );
    assert_eq!(
        EngineDeviceId::from_certificate_der(&vec![0_u8; MAX_CERTIFICATE_DER_BYTES + 1])
            .unwrap_err(),
        EngineConfigError::InvalidCertificate
    );
}

#[test]
fn initial_xml_is_network_inert_and_redacts_private_values() {
    let fixture = TempDir::new().expect("temporary directory");
    let root = fixture.path().join("folder");
    std::fs::create_dir(&root).expect("root");
    let config = desired(
        &fixture,
        vec![peer()],
        vec![folder(root.clone(), vec![id(PEER_ID), id(OWN_ID)])],
        Some(address(22000)),
    )
    .expect("config");
    let xml = config.render_initial_xml().expect("XML");

    assert!(
        xml.starts_with(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<configuration version=\"52\">"
        )
    );
    assert!(xml.contains(&format!(
        "<address>{}</address>",
        socket_path(&fixture).display()
    )));
    assert!(xml.contains("<unixSocketPermissions>0600</unixSocketPermissions>"));
    assert!(xml.contains("<listenAddress></listenAddress>"));
    assert!(!xml.contains("<folder "));
    assert!(!xml.contains(PEER_ID));
    assert!(!xml.contains("dynamic"));
    assert!(!xml.contains("syncthing.net"));
    assert!(!xml.contains("22000"));
    assert!(xml.contains("<globalAnnounceEnabled>false</globalAnnounceEnabled>"));
    assert!(xml.contains("<localAnnounceEnabled>false</localAnnounceEnabled>"));
    assert!(xml.contains("<relaysEnabled>false</relaysEnabled>"));
    assert!(xml.contains("<natEnabled>false</natEnabled>"));
    assert!(xml.contains("<startBrowser>false</startBrowser>"));
    assert!(xml.contains("<crashReportingEnabled>false</crashReportingEnabled>"));
    assert!(xml.contains("<autoUpgradeIntervalH>0</autoUpgradeIntervalH>"));

    let debug = format!("{config:?}");
    assert!(!debug.contains(config.api_key.expose()));
    assert!(!debug.contains(root.to_str().expect("UTF-8")));
    assert!(!debug.contains(socket_path(&fixture).to_str().expect("UTF-8")));
    assert!(debug.contains("[PRIVATE]"));
}

#[test]
fn desired_xml_is_deterministic_complete_and_escaped() {
    let fixture = TempDir::new().expect("temporary directory");
    let root = fixture.path().join("root & 'quote' <folder>");
    std::fs::create_dir(&root).expect("root");
    let peer = EnginePeerConfig::new(id(PEER_ID), "Peer & <remote> \"one\"", address(22001))
        .expect("peer")
        .with_paused(true);
    let folder = EngineFolderConfig::new(
        Uuid::from_u128(0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa),
        "Label & <private> \"quoted\"",
        root.clone(),
        vec![id(PEER_ID), id(OWN_ID)],
    )
    .expect("folder")
    .with_paused(true);
    let config = DesiredEngineConfig::new(
        id(OWN_ID),
        "Local & <device>",
        socket_path(&fixture),
        key(),
        Some(address(22000)),
        vec![peer],
        vec![folder],
    )
    .expect("config");
    let first = config.render_desired_xml().expect("XML");
    let second = config.render_desired_xml().expect("XML");

    assert_eq!(first.as_str(), second.as_str());
    assert!(first.contains("Local &amp; &lt;device&gt;"));
    assert!(first.contains("Peer &amp; &lt;remote&gt; &quot;one&quot;"));
    assert!(first.contains("Label &amp; &lt;private&gt; &quot;quoted&quot;"));
    assert!(first.contains("root &amp; &apos;quote&apos; &lt;folder&gt;"));
    assert!(first.contains("<listenAddress>tcp://127.0.0.1:22000</listenAddress>"));
    assert!(first.contains("<address>tcp://127.0.0.1:22001</address>"));
    assert!(first.contains("type=\"sendreceive\""));
    assert!(first.contains("ignorePerms=\"true\""));
    assert!(first.contains("<versioning type=\"simple\">"));
    assert!(first.contains("key=\"keep\" val=\"100\""));
    assert!(first.contains("key=\"cleanoutDays\" val=\"0\""));
    assert!(first.contains("<maxConflicts>-1</maxConflicts>"));
    assert!(first.contains("<disableFsync>false</disableFsync>"));
    assert_eq!(first.matches("<paused>true</paused>").count(), 2);
    assert!(config.peers()[0].paused());
    assert!(config.folders()[0].paused());
    assert_eq!(first.matches("<folder ").count(), 1);
    assert_eq!(first.matches("<device id=").count(), 4);
}

#[test]
fn verifies_retained_v2_1_3_effective_initial_and_desired_shapes() {
    let fixture = TempDir::new().expect("temporary directory");
    let root = fixture.path().join("shared");
    std::fs::create_dir(&root).expect("root");
    let config = desired(
        &fixture,
        vec![peer().with_paused(true)],
        vec![folder(root, vec![id(OWN_ID), id(PEER_ID)]).with_paused(true)],
        Some(address(22000)),
    )
    .expect("config");

    config
        .verify_initial_effective(&effective_fixture(&config, false))
        .expect("network-inert effective config");
    config
        .verify_effective(&effective_fixture(&config, true))
        .expect("desired effective config");
}

#[test]
fn effective_verification_rejects_security_membership_and_retention_drift() {
    let fixture = TempDir::new().expect("temporary directory");
    let root = fixture.path().join("shared");
    std::fs::create_dir(&root).expect("root");
    let config = desired(
        &fixture,
        vec![peer()],
        vec![folder(root, vec![id(OWN_ID), id(PEER_ID)])],
        Some(address(22000)),
    )
    .expect("config");
    let valid = effective_fixture(&config, true);

    let mutations = [
        ("/gui/address", json!("/tmp/foreign.sock")),
        ("/gui/apiKey", json!("00")),
        ("/gui/enabled", json!(false)),
        ("/gui/useTLS", json!(true)),
        ("/gui/sendBasicAuthPrompt", json!(true)),
        ("/gui/unixSocketPermissions", json!("0666")),
        ("/gui/metricsWithoutAuth", json!(true)),
        ("/gui/insecureAdminAccess", json!(true)),
        ("/gui/insecureSkipHostcheck", json!(true)),
        ("/gui/insecureAllowFrameLoading", json!(true)),
        ("/options/listenAddresses", json!(["tcp://0.0.0.0:22000"])),
        ("/options/globalAnnounceServers", json!(["default"])),
        ("/options/globalAnnounceEnabled", json!(true)),
        ("/options/localAnnounceEnabled", json!(true)),
        ("/options/relaysEnabled", json!(true)),
        ("/options/natEnabled", json!(true)),
        ("/options/startBrowser", json!(true)),
        ("/options/autoUpgradeIntervalH", json!(12)),
        ("/options/upgradeToPreReleases", json!(true)),
        ("/options/urAccepted", json!(3)),
        ("/options/urSeen", json!(0)),
        ("/options/urURL", json!("https://data.syncthing.net")),
        (
            "/options/releasesURL",
            json!("https://upgrades.syncthing.net"),
        ),
        ("/options/crashReportingEnabled", json!(true)),
        ("/options/crURL", json!("https://crash.syncthing.net")),
        ("/options/announceLANAddresses", json!(true)),
        ("/options/auditEnabled", json!(true)),
        ("/options/stunServers", json!(["default"])),
        ("/options/reconnectionIntervalS", json!(20)),
        ("/options/maxFolderConcurrency", json!(2)),
        ("/devices/0/name", json!("foreign")),
        ("/devices/0/addresses", json!(["tcp://127.0.0.1:9"])),
        ("/devices/0/compression", json!("always")),
        ("/devices/0/introducer", json!(true)),
        ("/devices/0/skipIntroductionRemovals", json!(true)),
        ("/devices/0/introducedBy", json!(PEER_ID)),
        ("/devices/1/paused", json!(true)),
        ("/devices/1/autoAcceptFolders", json!(true)),
        ("/devices/1/untrusted", json!(true)),
        ("/devices/1/numConnections", json!(2)),
        ("/folders/0/id", json!(Uuid::from_u128(2).to_string())),
        ("/folders/0/label", json!("foreign")),
        ("/folders/0/path", json!("/tmp/foreign")),
        ("/folders/0/type", json!("sendonly")),
        ("/folders/0/filesystemType", json!("fake")),
        ("/folders/0/rescanIntervalS", json!(60)),
        ("/folders/0/fsWatcherEnabled", json!(false)),
        ("/folders/0/fsWatcherDelayS", json!(1)),
        ("/folders/0/fsWatcherTimeoutS", json!(1)),
        ("/folders/0/paused", json!(true)),
        ("/folders/0/ignorePerms", json!(false)),
        ("/folders/0/autoNormalize", json!(false)),
        ("/folders/0/ignoreDelete", json!(true)),
        ("/folders/0/disableFsync", json!(true)),
        ("/folders/0/versioning/params/keep", json!("1")),
        ("/folders/0/versioning/params/cleanoutDays", json!("1")),
        ("/folders/0/versioning/type", json!("trashcan")),
        ("/folders/0/versioning/cleanupIntervalS", json!(60)),
        ("/folders/0/versioning/fsPath", json!("foreign")),
        ("/folders/0/versioning/fsType", json!("fake")),
        ("/folders/0/maxConflicts", json!(0)),
        ("/folders/0/markerName", json!(".foreign")),
        ("/folders/0/copyRangeMethod", json!("copy_file_range")),
        ("/folders/0/syncOwnership", json!(true)),
        ("/folders/0/sendOwnership", json!(true)),
        ("/folders/0/syncXattrs", json!(true)),
        ("/folders/0/sendXattrs", json!(true)),
        ("/folders/0/blockIndexing", json!(false)),
        ("/folders/0/minDiskFree/value", json!(0)),
        ("/folders/0/minDiskFree/unit", json!("B")),
    ];
    for (pointer, replacement) in mutations {
        let mut changed = valid.clone();
        *changed.pointer_mut(pointer).expect("fixture pointer") = replacement;
        assert_effective_mismatch(&config, &changed);
    }

    let mut missing = valid.clone();
    missing["options"]
        .as_object_mut()
        .expect("options")
        .remove("relaysEnabled");
    assert_effective_mismatch(&config, &missing);

    let mut wrong_type = valid.clone();
    wrong_type["folders"][0]["devices"] = json!({});
    assert_effective_mismatch(&config, &wrong_type);

    let mut extra_device = valid.clone();
    let duplicate = extra_device["devices"][1].clone();
    extra_device["devices"]
        .as_array_mut()
        .expect("devices")
        .push(duplicate);
    assert_effective_mismatch(&config, &extra_device);

    let mut duplicate_member = valid.clone();
    let member = duplicate_member["folders"][0]["devices"][0].clone();
    duplicate_member["folders"][0]["devices"]
        .as_array_mut()
        .expect("members")
        .push(member);
    assert_effective_mismatch(&config, &duplicate_member);
}

#[test]
fn initial_effective_verification_rejects_any_network_or_folder_activation() {
    let fixture = TempDir::new().expect("temporary directory");
    let root = fixture.path().join("shared");
    std::fs::create_dir(&root).expect("root");
    let config = desired(
        &fixture,
        vec![peer()],
        vec![folder(root, vec![id(OWN_ID), id(PEER_ID)])],
        Some(address(22000)),
    )
    .expect("config");
    let valid = effective_fixture(&config, false);

    for pointer in ["/options/listenAddresses", "/devices", "/folders"] {
        let mut changed = valid.clone();
        *changed.pointer_mut(pointer).expect("fixture pointer") = match pointer {
            "/options/listenAddresses" => json!(["tcp://127.0.0.1:22000"]),
            "/devices" => effective_fixture(&config, true)["devices"].clone(),
            "/folders" => effective_fixture(&config, true)["folders"].clone(),
            _ => unreachable!(),
        };
        assert_eq!(
            config.verify_initial_effective(&changed).unwrap_err(),
            EngineConfigError::EffectiveConfigMismatch
        );
    }
}

#[test]
fn sorts_devices_folders_and_members() {
    let fixture = TempDir::new().expect("temporary directory");
    let root_a = fixture.path().join("a");
    let root_b = fixture.path().join("b");
    std::fs::create_dir(&root_a).expect("root a");
    std::fs::create_dir(&root_b).expect("root b");
    let peer_a = id(PEER_ID);
    let peer_b = generated_id(42);
    let peers = vec![
        EnginePeerConfig::new(peer_b.clone(), "B", address(22002)).expect("B"),
        EnginePeerConfig::new(peer_a.clone(), "A", address(22001)).expect("A"),
    ];
    let folders = vec![
        EngineFolderConfig::new(
            Uuid::from_u128(2),
            "B",
            root_b,
            vec![peer_b.clone(), id(OWN_ID), peer_a.clone()],
        )
        .expect("B"),
        EngineFolderConfig::new(Uuid::from_u128(1), "A", root_a, vec![peer_a, id(OWN_ID)])
            .expect("A"),
    ];
    let config = desired(&fixture, peers, folders, Some(address(22000))).expect("config");

    assert!(config.peers.windows(2).all(|pair| pair[0].id < pair[1].id));
    assert!(
        config
            .folders
            .windows(2)
            .all(|pair| pair[0].id < pair[1].id)
    );
    assert!(
        config.folders[1]
            .members
            .windows(2)
            .all(|pair| pair[0] < pair[1])
    );
}

#[test]
fn rejects_unsafe_or_inconsistent_configs() {
    let fixture = TempDir::new().expect("temporary directory");
    let root = fixture.path().join("root");
    let child = root.join("child");
    let other = fixture.path().join("other");
    std::fs::create_dir(&root).expect("root");
    std::fs::create_dir(&child).expect("child");
    std::fs::create_dir(&other).expect("other");

    assert_eq!(
        EngineApiKey::parse(Zeroizing::new("not-a-key".to_owned())).unwrap_err(),
        EngineConfigError::InvalidApiKey
    );
    assert_eq!(
        EnginePeerConfig::new(id(PEER_ID), "", address(22001)).unwrap_err(),
        EngineConfigError::InvalidName
    );
    assert_eq!(
        EnginePeerConfig::new(
            id(PEER_ID),
            "peer",
            "0.0.0.0:22001".parse().expect("address")
        )
        .unwrap_err(),
        EngineConfigError::InvalidAddress
    );
    assert_eq!(
        DesiredEngineConfig::new(
            id(OWN_ID),
            "local",
            PathBuf::from("relative.sock"),
            key(),
            None,
            Vec::new(),
            Vec::new(),
        )
        .unwrap_err(),
        EngineConfigError::InvalidSocketPath
    );
    assert_eq!(
        EngineFolderConfig::new(
            Uuid::from_u128(1),
            "missing",
            fixture.path().join("absent"),
            vec![id(OWN_ID), id(PEER_ID)],
        )
        .unwrap_err(),
        EngineConfigError::UnsafeFolderRoot
    );
    assert_eq!(
        EngineFolderConfig::new(
            Uuid::from_u128(1),
            "filesystem root",
            PathBuf::from("/"),
            vec![id(OWN_ID), id(PEER_ID)],
        )
        .unwrap_err(),
        EngineConfigError::UnsafeFolderRoot
    );

    let duplicate_peer = peer();
    assert_eq!(
        desired(
            &fixture,
            vec![duplicate_peer.clone(), duplicate_peer],
            Vec::new(),
            None,
        )
        .unwrap_err(),
        EngineConfigError::DuplicateDevice
    );
    assert_eq!(
        desired(
            &fixture,
            vec![EnginePeerConfig::new(id(OWN_ID), "self", address(22001)).expect("peer")],
            Vec::new(),
            None,
        )
        .unwrap_err(),
        EngineConfigError::SelfPeer
    );
    assert_eq!(
        desired(
            &fixture,
            vec![peer()],
            vec![folder(root.clone(), vec![id(OWN_ID), generated_id(7)])],
            Some(address(22000)),
        )
        .unwrap_err(),
        EngineConfigError::UnknownMember
    );
    assert_eq!(
        desired(
            &fixture,
            vec![peer()],
            vec![folder(root.clone(), vec![id(OWN_ID), id(PEER_ID)])],
            None,
        )
        .unwrap_err(),
        EngineConfigError::MissingListener
    );
    assert_eq!(
        desired(
            &fixture,
            vec![peer()],
            vec![folder(root.clone(), vec![id(OWN_ID)])],
            Some(address(22000)),
        )
        .unwrap_err(),
        EngineConfigError::MissingPeer
    );
    assert_eq!(
        desired(
            &fixture,
            vec![peer()],
            vec![
                folder(root.clone(), vec![id(OWN_ID), id(PEER_ID)]),
                EngineFolderConfig::new(
                    Uuid::from_u128(2),
                    "child",
                    child,
                    vec![id(OWN_ID), id(PEER_ID)],
                )
                .expect("folder"),
            ],
            Some(address(22000)),
        )
        .unwrap_err(),
        EngineConfigError::OverlappingRoots
    );

    let duplicate_id = Uuid::from_u128(3);
    let duplicate_folders = vec![
        EngineFolderConfig::new(duplicate_id, "one", root, vec![id(OWN_ID), id(PEER_ID)])
            .expect("folder"),
        EngineFolderConfig::new(duplicate_id, "two", other, vec![id(OWN_ID), id(PEER_ID)])
            .expect("folder"),
    ];
    assert_eq!(
        desired(
            &fixture,
            vec![peer()],
            duplicate_folders,
            Some(address(22000)),
        )
        .unwrap_err(),
        EngineConfigError::DuplicateFolder
    );

    assert_eq!(
        desired(
            &fixture,
            vec![peer()],
            vec![folder(
                fixture.path().to_path_buf(),
                vec![id(OWN_ID), id(PEER_ID)],
            )],
            Some(address(22000)),
        )
        .unwrap_err(),
        EngineConfigError::UnsafeFolderRoot
    );
}

#[test]
fn rejects_canonical_root_text_that_xml_cannot_preserve() {
    use std::os::unix::fs::symlink;

    let fixture = TempDir::new().expect("temporary directory");
    let unsafe_root = fixture.path().join("unsafe\nroot");
    let alias = fixture.path().join("safe-alias");
    std::fs::create_dir(&unsafe_root).expect("unsafe-name root");
    symlink(&unsafe_root, &alias).expect("root symlink");

    assert_eq!(
        EngineFolderConfig::new(
            Uuid::from_u128(1),
            "folder",
            alias,
            vec![id(OWN_ID), id(PEER_ID)],
        )
        .unwrap_err(),
        EngineConfigError::UnsafeFolderRoot
    );
}

#[test]
fn bounds_peer_count_before_rendering() {
    let fixture = TempDir::new().expect("temporary directory");
    let peers = (0..=MAX_PEERS)
        .map(|index| {
            EnginePeerConfig::new(generated_id(index), "peer", address(22001)).expect("peer")
        })
        .collect();
    assert_eq!(
        desired(&fixture, peers, Vec::new(), None).unwrap_err(),
        EngineConfigError::LimitExceeded
    );
}

#[test]
fn bounds_total_members_and_keeps_boundary_xml_below_client_config_budget() {
    let fixture = TempDir::new().expect("temporary directory");
    let peers = (0..32)
        .map(|index| {
            EnginePeerConfig::new(generated_id(index), "peer", address(22001)).expect("peer")
        })
        .collect::<Vec<_>>();
    let boundary_members = std::iter::once(id(OWN_ID))
        .chain(peers.iter().take(31).map(|peer| peer.id.clone()))
        .collect::<Vec<_>>();
    let mut folders = Vec::new();
    for index in 0..128_u128 {
        let root = fixture.path().join(format!("folder-{index}"));
        std::fs::create_dir(&root).expect("root");
        folders.push(
            EngineFolderConfig::new(
                Uuid::from_u128(index + 1),
                "folder",
                root,
                boundary_members.clone(),
            )
            .expect("folder"),
        );
    }
    let boundary = desired(
        &fixture,
        peers.clone(),
        folders.clone(),
        Some(address(22000)),
    )
    .expect("boundary");
    let xml = boundary.render_desired_xml().expect("boundary XML");
    assert_eq!(
        boundary
            .folders()
            .iter()
            .map(|folder| folder.members().len())
            .sum::<usize>(),
        MAX_TOTAL_FOLDER_MEMBERS
    );
    assert!(xml.len() < 1024 * 1024, "actual XML bytes: {}", xml.len());

    folders[0].members.push(peers[31].id.clone());
    assert_eq!(
        desired(&fixture, peers, folders, Some(address(22000))).unwrap_err(),
        EngineConfigError::LimitExceeded
    );
}

#[test]
fn getters_return_only_validated_nonsecret_state() {
    let fixture = TempDir::new().expect("temporary directory");
    let root = fixture.path().join("root");
    std::fs::create_dir(&root).expect("root");
    let config = desired(
        &fixture,
        vec![peer()],
        vec![folder(root.clone(), vec![id(OWN_ID), id(PEER_ID)])],
        Some(address(22000)),
    )
    .expect("config");

    assert_eq!(config.own_id().as_str(), OWN_ID);
    assert_eq!(config.own_name(), "Local device");
    assert_eq!(config.gui_socket(), socket_path(&fixture));
    assert_eq!(config.listener(), Some(address(22000)));
    assert_eq!(config.peers()[0].id().as_str(), PEER_ID);
    assert_eq!(config.peers()[0].name(), "Peer");
    assert_eq!(config.peers()[0].address(), address(22001));
    assert!(!config.peers()[0].paused());
    assert_eq!(config.folders()[0].label(), "Shared folder");
    assert_eq!(
        config.folders()[0].root(),
        root.canonicalize().expect("canonical")
    );
    assert_eq!(config.folders()[0].members().len(), 2);
    assert!(!config.folders()[0].paused());
    assert_eq!(config.api_key_copy().len(), API_KEY_BYTES);
}

fn generated_id(seed: usize) -> EngineDeviceId {
    let mut payload = [b'A'; 52];
    payload[0] = LUHN_ALPHABET[1 + seed % 31];
    payload[1] = LUHN_ALPHABET[(seed / 31) % 32];
    EngineDeviceId::parse(&canonical_id(payload)).expect("generated valid ID")
}

fn canonical_id(payload: [u8; 52]) -> String {
    let mut checked = Vec::with_capacity(56);
    for block in payload.chunks_exact(13) {
        checked.extend_from_slice(block);
        checked.push(luhn32(block).expect("alphabet"));
    }
    let mut canonical = String::with_capacity(63);
    for (index, block) in checked.chunks_exact(7).enumerate() {
        if index > 0 {
            canonical.push('-');
        }
        canonical.push_str(std::str::from_utf8(block).expect("ASCII"));
    }
    canonical
}
