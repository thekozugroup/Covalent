use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read as _, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command as ProcessCommand, ExitStatus, Stdio};
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use clap::{Parser, Subcommand, ValueEnum};
use covalent_core::{
    AuthorizedRoot, BackupOptions, Engine, EngineOptions, JobControl, PairingSession,
    RestoreOptions, RestorePlan, StaticKeyProtector,
};
use covalent_node::first_run_claim::{
    CLAIM_NONCE_BYTES, client_proof, normalise_claim_code, open_sealed_token, stretch_claim_code,
    validate_exact_ca_certificate_pem,
};
use covalent_protocol::{
    BackupId, ConflictPolicy, DeviceId, ExportedDeviceSettings, PROTOCOL_VERSION,
    PairingInvitation, PeerRole, RelativePath, ReplicaAvailability, ReplicaIntent,
};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize, de};
use sha2::{Digest as _, Sha256};
use tempfile::{NamedTempFile, TempDir};
use zeroize::{Zeroize, Zeroizing};

#[derive(Debug, Parser)]
#[command(name = "covalent", version, about = "Covalent backup operator CLI")]
struct Arguments {
    /// Durable Covalent state directory.
    #[arg(
        long,
        env = "COVALENT_DATA_DIR",
        default_value = ".covalent-data",
        global = true
    )]
    data_dir: PathBuf,
    /// Owner-readable file containing the explicitly provisioned base64url KEK.
    #[arg(
        long,
        env = "COVALENT_KEY_ENCRYPTION_KEY_FILE",
        global = true,
        value_name = "PATH"
    )]
    key_encryption_key_file: Option<PathBuf>,
    /// KEK version for newly written state. Keep it unchanged until a rotation tool ships.
    #[arg(
        long,
        env = "COVALENT_KEY_ENCRYPTION_KEY_VERSION",
        global = true,
        default_value_t = 1
    )]
    key_encryption_key_version: u32,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Claim a fresh HTTPS node, verify its exact CA and hostname, and save local credentials.
    Claim {
        /// Exact HTTPS origin, for example <https://atlas.example-tailnet.ts.net:8443>.
        #[arg(long, value_name = "HTTPS_URL")]
        https_url: String,
        /// Owner-readable file containing the one-time setup code. Never pass the code as an argument.
        #[arg(long, value_name = "PATH")]
        setup_code_file: PathBuf,
        /// New directory where root.crt and local-api-token will be created mode 0600.
        #[arg(long, value_name = "PATH")]
        output_dir: PathBuf,
    },
    /// Prints deterministic product and safety diagnostics.
    Doctor,
    /// Prints live non-secret engine state.
    Status,
    /// Lists authoritative remembered backups and latest committed snapshots.
    Backups,
    /// Starts the long-running daemon binary with the same state directory.
    Daemon {
        /// Local HTTP listen socket.
        #[arg(long, default_value = "127.0.0.1:8787")]
        listen: String,
        /// QUIC peer UDP listen socket.
        #[arg(long, default_value = "127.0.0.1:8787")]
        peer_listen: String,
    },
    /// Applies production relative-path and root checks without writing.
    ValidateRestorePath {
        /// Existing directory explicitly authorized by the user.
        #[arg(long)]
        root: PathBuf,
        /// Slash-separated path relative to the authorized root.
        #[arg(long)]
        relative: String,
    },
    /// Prints an export-safe settings example with no identity key.
    SettingsExample {
        /// Device name included in the example.
        #[arg(long, default_value = "My Covalent device")]
        device_name: String,
    },
    /// Creates an expiring signed pairing invitation.
    PairInvite {
        /// Invitation lifetime in seconds, capped at 15 minutes.
        #[arg(long, default_value_t = 300)]
        lifetime_seconds: u64,
        /// Candidate address hint; identity is still cryptographically verified.
        #[arg(long = "endpoint")]
        endpoints: Vec<String>,
    },
    /// Accepts an invitation and emits a role-bound exchange plus comparison code.
    PairAccept {
        /// Signed invitation JSON file from the inviter.
        #[arg(long)]
        invitation: PathBuf,
        /// User-visible name for this responding device.
        #[arg(long)]
        name: String,
        /// Roles the inviter will grant this responder.
        #[arg(long = "responder-role", value_enum)]
        responder_roles: Vec<Role>,
        /// Roles this responder will grant the inviter.
        #[arg(long = "inviter-role", value_enum)]
        inviter_roles: Vec<Role>,
        /// Session output file; stdout is used when omitted.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Signs one device's explicit comparison-code confirmation.
    PairConfirm {
        #[arg(long)]
        session: PathBuf,
        #[arg(long, value_enum)]
        side: PairSide,
        /// Exact four-group code displayed on both physical devices.
        #[arg(long)]
        code: String,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Finalizes one side's grant after both signed confirmations exist.
    PairFinalize {
        #[arg(long)]
        session: PathBuf,
        #[arg(long, value_enum)]
        side: PairSide,
    },
    /// Revokes one remembered peer and emits a signed roster epoch.
    PairRevoke {
        /// Stable paired device UUID.
        #[arg(long)]
        peer_id: String,
    },
    /// Streams, chunks, encrypts, signs, and durably commits a backup.
    Backup {
        /// Explicit source directory.
        #[arg(long)]
        source: PathBuf,
        /// User-visible remembered backup name.
        #[arg(long)]
        name: String,
        /// Existing logical backup UUID; omitted creates one.
        #[arg(long)]
        backup_id: Option<String>,
        /// Monotonic caller-provided snapshot identifier.
        #[arg(long)]
        snapshot_id: String,
        /// Stable resumable job identifier. Omitted derives one from the immutable request.
        #[arg(long)]
        job_id: Option<String>,
        /// Exact explicitly selected provider UUID. No provider is auto-selected.
        #[arg(long = "provider")]
        providers: Vec<String>,
    },
    /// Authenticates and digest-checks a committed snapshot.
    Verify {
        #[arg(long)]
        backup_id: String,
        #[arg(long)]
        snapshot_id: String,
        /// Repair local corruption from intact connected authorized providers.
        #[arg(long, default_value_t = false)]
        repair: bool,
        /// Verify acknowledged copies on connected selected providers.
        #[arg(long, default_value_t = false)]
        providers: bool,
    },
    /// Creates a signed no-write restore plan.
    RestorePreview {
        #[arg(long)]
        backup_id: String,
        #[arg(long)]
        snapshot_id: String,
        /// Existing user-authorized target root.
        #[arg(long)]
        target: PathBuf,
        #[arg(long, value_enum, default_value_t = Conflict::Fail)]
        conflict: Conflict,
        #[arg(long, default_value = "cli-restore")]
        job_id: String,
        /// Optional plan file; stdout is used when omitted.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Executes only an exact signed restore plan produced by preview.
    RestoreExecute {
        /// Signed plan JSON file.
        #[arg(long)]
        plan: PathBuf,
    },
    /// Exports non-secret versioned settings.
    ConfigExport {
        /// Optional output file; stdout is used when omitted.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Imports versioned non-secret settings.
    ConfigImport {
        #[arg(long)]
        input: PathBuf,
        /// Required explicit replacement confirmation.
        #[arg(long, default_value_t = false)]
        confirm: bool,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Conflict {
    Fail,
    Skip,
    Replace,
    Rename,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Role {
    StorageProvider,
    BackupReader,
    BackupWriter,
}

impl From<Role> for PeerRole {
    fn from(value: Role) -> Self {
        match value {
            Role::StorageProvider => Self::StorageProvider,
            Role::BackupReader => Self::BackupReader,
            Role::BackupWriter => Self::BackupWriter,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum PairSide {
    Inviter,
    Responder,
}

impl From<Conflict> for ConflictPolicy {
    fn from(value: Conflict) -> Self {
        match value {
            Conflict::Fail => Self::Fail,
            Conflict::Skip => Self::Skip,
            Conflict::Replace => Self::Replace,
            Conflict::Rename => Self::Rename,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DoctorReport {
    status: &'static str,
    protocol_version: u16,
    memory_safe_engine: bool,
    authenticated_encryption: bool,
    signed_manifests: bool,
    authenticated_quic: bool,
    external_account_required: bool,
    automatic_replica_placement: bool,
    restore_requires_authorized_root: bool,
    tier1: [&'static str; 4],
    tier2: [&'static str; 1],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusReport {
    device_id: String,
    device_name: String,
    protocol_version: u16,
    lan_discovery_enabled: bool,
    remembered_backups: usize,
    trusted_peers: usize,
    revoked_peers: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BackupReport {
    backup_id: String,
    snapshot_id: String,
    entries: usize,
    bytes_read: u64,
    chunks_stored: usize,
    chunks_deduplicated: usize,
    selected_providers: usize,
    provider_failures: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VerifyReport {
    verified: usize,
    missing: Vec<String>,
    corrupt: Vec<String>,
    intact: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider_availability: Option<BTreeMap<DeviceId, ReplicaAvailability>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RestoreReport {
    files_restored: usize,
    directories_created: usize,
    files_skipped: usize,
    bytes_written: u64,
    rejected_provider_copies: usize,
}

fn main() -> Result<()> {
    let Arguments {
        data_dir,
        key_encryption_key_file,
        key_encryption_key_version,
        command,
    } = Arguments::parse();
    let key_protection = KeyProtectionConfiguration {
        key_file: key_encryption_key_file,
        key_version: key_encryption_key_version,
    };
    match command {
        Command::Claim {
            https_url,
            setup_code_file,
            output_dir,
        } => claim_https_node(&https_url, &setup_code_file, &output_dir),
        Command::Doctor => print_json(&DoctorReport {
            status: "ok",
            protocol_version: PROTOCOL_VERSION,
            memory_safe_engine: true,
            authenticated_encryption: true,
            signed_manifests: true,
            authenticated_quic: true,
            external_account_required: false,
            automatic_replica_placement: false,
            restore_requires_authorized_root: true,
            tier1: ["macOS", "Android", "Docker", "Unraid"],
            tier2: ["iOS"],
        }),
        Command::Status => {
            let engine = open_engine(data_dir, &key_protection)?;
            let config = engine.config()?;
            print_json(&StatusReport {
                device_id: engine.device_id().to_string(),
                device_name: config.device_name,
                protocol_version: PROTOCOL_VERSION,
                lan_discovery_enabled: config.lan_discovery_enabled,
                remembered_backups: config.remembered_backups.len(),
                trusted_peers: config
                    .trusted_peers
                    .values()
                    .filter(|grant| !grant.revoked)
                    .count(),
                revoked_peers: config
                    .trusted_peers
                    .values()
                    .filter(|grant| grant.revoked)
                    .count(),
            })
        }
        Command::Backups => {
            let engine = open_engine(data_dir, &key_protection)?;
            print_json(&engine.list_backups()?)
        }
        Command::Daemon {
            listen,
            peer_listen,
        } => {
            let mut command = ProcessCommand::new("covalent-node");
            command
                .args([
                    "serve",
                    "--listen",
                    &listen,
                    "--peer-listen",
                    &peer_listen,
                    "--data-dir",
                ])
                .arg(data_dir)
                .arg("--key-encryption-key-version")
                .arg(key_protection.key_version.to_string());
            if let Some(key_file) = key_protection.key_file.as_deref() {
                command.arg("--key-encryption-key-file").arg(key_file);
            }
            let status = command.status().context("start covalent-node daemon")?;
            if !status.success() {
                bail!("covalent-node exited with {status}");
            }
            Ok(())
        }
        Command::ValidateRestorePath { root, relative } => {
            let root = AuthorizedRoot::open(&root)
                .with_context(|| format!("authorize restore root {}", root.display()))?;
            let relative = RelativePath::new(relative)?;
            let destination = root.resolve(&relative)?;
            println!("{}", destination.display());
            Ok(())
        }
        Command::SettingsExample { device_name } => {
            let settings = ExportedDeviceSettings::new(device_name, false, Vec::new())?;
            print_json(&settings)
        }
        Command::PairInvite {
            lifetime_seconds,
            endpoints,
        } => {
            let engine = open_engine(data_dir, &key_protection)?;
            let invitation = engine.pairing_manager().create_invitation(
                now_unix_ms(),
                lifetime_seconds.saturating_mul(1_000),
                endpoints,
            )?;
            print_json(&invitation)
        }
        Command::PairAccept {
            invitation,
            name,
            responder_roles,
            inviter_roles,
            output,
        } => {
            let engine = open_engine(data_dir, &key_protection)?;
            let invitation: PairingInvitation =
                serde_json::from_slice(&read_bounded(&invitation, 1_048_576)?)?;
            let session = engine.accept_pairing(
                invitation,
                name,
                responder_roles.into_iter().map(PeerRole::from).collect(),
                inviter_roles.into_iter().map(PeerRole::from).collect(),
                now_unix_ms(),
            )?;
            write_or_print(output.as_deref(), &serde_json::to_vec_pretty(&session)?)
        }
        Command::PairConfirm {
            session,
            side,
            code,
            output,
        } => {
            let engine = open_engine(data_dir, &key_protection)?;
            let mut session: PairingSession =
                serde_json::from_slice(&read_bounded(&session, 1_048_576)?)?;
            match side {
                PairSide::Inviter => {
                    engine.confirm_pairing_as_inviter(&mut session, &code, now_unix_ms())?
                }
                PairSide::Responder => {
                    engine.confirm_pairing_as_responder(&mut session, &code, now_unix_ms())?
                }
            }
            write_or_print(output.as_deref(), &serde_json::to_vec_pretty(&session)?)
        }
        Command::PairFinalize { session, side } => {
            let engine = open_engine(data_dir, &key_protection)?;
            let session: PairingSession =
                serde_json::from_slice(&read_bounded(&session, 1_048_576)?)?;
            let confirmation = match side {
                PairSide::Inviter => engine.finalize_pairing_as_inviter(&session, now_unix_ms())?,
                PairSide::Responder => {
                    engine.finalize_pairing_as_responder(&session, now_unix_ms())?
                }
            };
            print_json(&confirmation)
        }
        Command::PairRevoke { peer_id } => {
            let engine = open_engine(data_dir, &key_protection)?;
            let roster = engine.revoke_peer(parse_device_id(&peer_id)?)?;
            print_json(&roster)
        }
        Command::Backup {
            source,
            name,
            backup_id,
            snapshot_id,
            job_id,
            providers,
        } => {
            let engine = open_engine(data_dir, &key_protection)?;
            let job_id = job_id.unwrap_or_else(|| {
                default_backup_job_id(
                    &source,
                    &name,
                    backup_id.as_deref(),
                    &snapshot_id,
                    &providers,
                )
            });
            let requested_backup_id = backup_id.as_deref().map(parse_backup_id).transpose()?;
            let backup_id = requested_backup_id
                .or(engine.unacknowledged_backup_id(&job_id)?)
                .unwrap_or_default();
            let selected: Result<Vec<_>> = providers
                .iter()
                .map(|provider| parse_device_id(provider))
                .collect();
            let mut options = BackupOptions::new(backup_id, snapshot_id, &job_id);
            options.display_name = name;
            options.created_at_unix_ms = now_unix_ms();
            options.replica_intent = ReplicaIntent::explicit(selected?);
            let result = engine.backup(source, &options, &JobControl::new(), |_| {})?;
            let report = BackupReport {
                backup_id: backup_id.to_string(),
                snapshot_id: result.manifest.snapshot_id,
                entries: result.manifest.entries.len(),
                bytes_read: result.progress.bytes_read,
                chunks_stored: result.progress.chunks_stored,
                chunks_deduplicated: result.progress.chunks_deduplicated,
                selected_providers: result.manifest.replica_intent.selected_providers.len(),
                provider_failures: result.replication.failures.len(),
            };
            let stdout = std::io::stdout();
            let mut output = stdout.lock();
            write_backup_report_and_acknowledge(&engine, &job_id, &report, &mut output)
        }
        Command::Verify {
            backup_id,
            snapshot_id,
            repair,
            providers,
        } => {
            let engine = open_engine(data_dir, &key_protection)?;
            let backup_id = parse_backup_id(&backup_id)?;
            if repair {
                engine.repair_snapshot(backup_id, &snapshot_id)?;
            }
            let (report, provider_availability) = if providers {
                let availability = engine.verify_snapshot_availability(backup_id, &snapshot_id)?;
                (availability.local, Some(availability.providers))
            } else {
                (engine.verify_snapshot(backup_id, &snapshot_id)?, None)
            };
            let intact = report.is_intact();
            print_json(&VerifyReport {
                verified: report.verified,
                missing: report.missing,
                corrupt: report.corrupt,
                intact,
                provider_availability,
            })
        }
        Command::RestorePreview {
            backup_id,
            snapshot_id,
            target,
            conflict,
            job_id,
            output,
        } => {
            let engine = open_engine(data_dir, &key_protection)?;
            let plan = engine.preview_restore(
                parse_backup_id(&backup_id)?,
                &snapshot_id,
                target,
                &RestoreOptions {
                    conflict_policy: conflict.into(),
                    selected_paths: Default::default(),
                    job_id,
                    target_inventory: None,
                },
            )?;
            let bytes = serde_json::to_vec_pretty(&plan)?;
            write_or_print(output.as_deref(), &bytes)
        }
        Command::RestoreExecute { plan } => {
            let engine = open_engine(data_dir, &key_protection)?;
            let bytes = read_bounded(&plan, 2 * 1_024 * 1_024)?;
            let plan: RestorePlan = serde_json::from_slice(&bytes)?;
            let report = engine.restore(&plan, &JobControl::new())?;
            print_json(&RestoreReport {
                files_restored: report.files_restored,
                directories_created: report.directories_created,
                files_skipped: report.files_skipped,
                bytes_written: report.bytes_written,
                rejected_provider_copies: report.rejected_provider_copies.len(),
            })
        }
        Command::ConfigExport { output } => {
            let engine = open_engine(data_dir, &key_protection)?;
            write_or_print(output.as_deref(), &engine.export_settings()?)
        }
        Command::ConfigImport { input, confirm } => {
            let engine = open_engine(data_dir, &key_protection)?;
            let bytes = read_bounded(&input, 1_048_576)?;
            engine.import_settings(&bytes, confirm)?;
            println!("settings imported");
            Ok(())
        }
    }
}

#[derive(Clone, Debug)]
struct KeyProtectionConfiguration {
    key_file: Option<PathBuf>,
    key_version: u32,
}

fn open_engine(
    data_directory: PathBuf,
    key_protection: &KeyProtectionConfiguration,
) -> Result<Engine> {
    let key_file = key_protection.key_file.as_deref().context(
        "key protection is locked: set COVALENT_KEY_ENCRYPTION_KEY_FILE to a provisioned owner-readable key file; run covalent-node provision-key --key-file <path> first",
    )?;
    if key_protection.key_version == 0 {
        bail!("COVALENT_KEY_ENCRYPTION_KEY_VERSION must be greater than zero");
    }
    let encoded = read_owner_key_file(key_file)?;
    let protector = StaticKeyProtector::from_base64(key_protection.key_version, &encoded)
        .context("load explicitly provisioned KEK")?;
    Engine::open(EngineOptions::new(data_directory).with_key_protector(Arc::new(protector)))
        .context("open Covalent engine")
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClaimResponse {
    #[serde(rename = "deviceName")]
    _device_name: String,
    ca_certificate: Option<ClaimCaPem>,
    ca_fingerprint_sha256: Option<String>,
    seal_nonce: String,
    sealed_token: String,
}

const MAX_CLAIM_CA_PEM_BYTES: usize = 64 * 1_024;
const CLAIM_ATTEMPT_SCHEMA_VERSION: u16 = 1;
const MAX_CLAIM_ATTEMPT_BYTES: u64 = 4 * 1_024;
const CLAIM_ATTEMPT_PATH_DOMAIN: &[u8] = b"covalent/claim-attempt/output-path/v1";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClaimAttemptJournal {
    schema_version: u16,
    output_path_digest: String,
    client_nonce: String,
    client_proof: String,
}

impl Drop for ClaimAttemptJournal {
    fn drop(&mut self) {
        self.client_nonce.zeroize();
        self.client_proof.zeroize();
    }
}

/// A bounded, untrusted PEM string.  Deserializing through a visitor keeps an
/// oversized `caCertificate` from becoming a separately allocated `String`.
#[derive(Debug)]
struct ClaimCaPem(String);

impl<'de> Deserialize<'de> for ClaimCaPem {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        struct ClaimCaPemVisitor;

        impl<'de> de::Visitor<'de> for ClaimCaPemVisitor {
            type Value = ClaimCaPem;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a PEM CA certificate no larger than 64 KiB")
            }

            fn visit_borrowed_str<E>(self, value: &'de str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Self::visit_str(self, value)
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                if value.len() > MAX_CLAIM_CA_PEM_BYTES {
                    return Err(E::custom("CA certificate exceeds the 64 KiB limit"));
                }
                Ok(ClaimCaPem(value.to_owned()))
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                if value.len() > MAX_CLAIM_CA_PEM_BYTES {
                    return Err(E::custom("CA certificate exceeds the 64 KiB limit"));
                }
                Ok(ClaimCaPem(value))
            }
        }

        deserializer.deserialize_str(ClaimCaPemVisitor)
    }
}

#[derive(Debug)]
struct VerifiedClaimCa {
    canonical_pem: String,
    digest: [u8; 32],
}

/// Claims a node only through the one-shot protocol, then proves that the
/// returned CA validates the exact user-supplied hostname before saving either
/// credential.  Curl's one insecure request is tightly confined to the claim
/// endpoint; the sealed token makes a relay unable to substitute the CA.
fn claim_https_node(https_url: &str, setup_code_file: &Path, output_dir: &Path) -> Result<()> {
    let https_url = normalise_https_origin(https_url)?;
    let output_dir = validated_claim_output_path(output_dir)?;
    let output_parent = output_dir
        .parent()
        .context("claim output directory must have an existing parent directory")?;
    let code = read_setup_code(setup_code_file)?;
    let journal_path = claim_attempt_journal_path(&output_dir)?;
    let existing_attempt = load_claim_attempt(&journal_path)?;
    match fs::symlink_metadata(&output_dir) {
        Ok(_) => {
            let attempt = existing_attempt.context(
                "claim output directory already exists without a matching pending claim; choose a new path",
            )?;
            validate_claim_attempt(&attempt, &output_dir, &code)?;
            reconcile_published_claim_credentials(&https_url, &output_dir)?;
            remove_claim_attempt(&journal_path, &attempt)?;
            println!(
                "Claimed node. Recovered and verified credentials in {}.",
                output_dir.display()
            );
            return Ok(());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("inspect {}", output_dir.display()));
        }
    }

    let attempt = match existing_attempt {
        Some(attempt) => {
            validate_claim_attempt(&attempt, &output_dir, &code)?;
            attempt
        }
        None => create_claim_attempt(&journal_path, &output_dir, &code)?,
    };
    let output_directory = prepare_claim_output_directory(&output_dir)?;

    let claim_key = stretch_claim_code(&code);
    let client_nonce = decode_claim_attempt_nonce(&attempt)?;
    let request = serde_json::json!({
        "clientNonce": attempt.client_nonce,
        "clientProof": attempt.client_proof,
    });
    let response = curl_bootstrap_claim(&format!("{https_url}/api/v1/claim"), &request)?;
    let response = parse_claim_response(&response)?;
    let ca_pem = response.ca_certificate.context(
        "claim response did not include a CA certificate; this command requires HTTPS exact-CA trust",
    )?;
    let ca_fingerprint = response
        .ca_fingerprint_sha256
        .context("claim response did not include a CA fingerprint; refusing to save credentials")?;
    let verified_ca = verify_claim_ca(&ca_pem.0, &ca_fingerprint)?;
    let seal_nonce = URL_SAFE_NO_PAD
        .decode(&response.seal_nonce)
        .context("claim response has an invalid sealing nonce")?;
    let sealed_token = URL_SAFE_NO_PAD
        .decode(&response.sealed_token)
        .context("claim response has an invalid sealed token")?;
    let token = open_sealed_token(
        &claim_key,
        client_nonce.as_ref(),
        &verified_ca.digest,
        &seal_nonce,
        &sealed_token,
    )
    .context("claim response could not be decrypted with this setup code and CA")?;
    if token.len() < 32 || token.len() > 512 {
        bail!("claim response contains an invalid local API token");
    }

    let temporary_ca = write_private_temporary_file(
        output_directory.path(),
        verified_ca.canonical_pem.as_bytes(),
    )?;
    curl_verify_ca_hostname_and_token(
        &format!("{https_url}/api/v1/backups"),
        temporary_ca.path(),
        token.as_bytes(),
        output_directory.path(),
    )?;
    persist_claim_credentials(output_directory.path(), temporary_ca, token.as_bytes())?;
    sync_directory(output_directory.path())?;
    if fs::symlink_metadata(&output_dir).is_ok() {
        bail!(
            "claim output directory appeared during setup; credentials remain private and were not published"
        );
    }
    fs::rename(output_directory.path(), &output_dir)
        .context("publish claimed credentials without replacing an existing directory")?;
    let _ = output_directory.keep();
    sync_directory(output_parent)?;
    remove_claim_attempt(&journal_path, &attempt)?;
    println!(
        "Claimed node. Saved exact CA and local API token in {}. Keep both files private; the web console accepts the token only.",
        output_dir.display()
    );
    Ok(())
}

fn validated_claim_output_path(path: &Path) -> Result<PathBuf> {
    let name = path
        .file_name()
        .context("claim output directory must name a new directory")?;
    let parent = path
        .parent()
        .context("claim output directory must have an existing parent directory")?;
    let metadata = fs::symlink_metadata(parent)
        .with_context(|| format!("inspect claim output parent {}", parent.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "claim output parent must be a real directory: {}",
            parent.display()
        );
    }
    require_private_directory_permissions(&metadata, parent)?;
    let parent = fs::canonicalize(parent)
        .with_context(|| format!("canonicalize claim output parent {}", parent.display()))?;
    Ok(parent.join(name))
}

fn claim_output_path_digest(path: &Path) -> String {
    let mut digest = Sha256::new();
    digest.update(CLAIM_ATTEMPT_PATH_DOMAIN);
    digest.update((path.as_os_str().as_encoded_bytes().len() as u64).to_be_bytes());
    digest.update(path.as_os_str().as_encoded_bytes());
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn claim_attempt_journal_path(output_dir: &Path) -> Result<PathBuf> {
    let parent = output_dir
        .parent()
        .context("claim output directory must have an existing parent directory")?;
    Ok(parent.join(format!(
        ".covalent-claim-attempt-{}.json",
        claim_output_path_digest(output_dir)
    )))
}

fn claim_attempt_proof(code: &str, client_nonce: &[u8]) -> Zeroizing<[u8; 32]> {
    let key = stretch_claim_code(code);
    Zeroizing::new(client_proof(&key, client_nonce))
}

fn decode_claim_attempt_nonce(attempt: &ClaimAttemptJournal) -> Result<Zeroizing<Vec<u8>>> {
    let nonce = Zeroizing::new(
        URL_SAFE_NO_PAD
            .decode(&attempt.client_nonce)
            .context("saved claim attempt has an invalid nonce")?,
    );
    if nonce.len() != CLAIM_NONCE_BYTES {
        bail!("saved claim attempt has an invalid nonce");
    }
    Ok(nonce)
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    let mut difference = u8::from(left.len() != right.len());
    for index in 0..left.len().max(right.len()) {
        difference |=
            left.get(index).copied().unwrap_or(0) ^ right.get(index).copied().unwrap_or(0);
    }
    difference == 0
}

fn validate_claim_attempt(
    attempt: &ClaimAttemptJournal,
    output_dir: &Path,
    code: &str,
) -> Result<()> {
    if attempt.schema_version != CLAIM_ATTEMPT_SCHEMA_VERSION
        || attempt.output_path_digest.len() != 64
        || !attempt
            .output_path_digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        || !constant_time_equal(
            attempt.output_path_digest.as_bytes(),
            claim_output_path_digest(output_dir).as_bytes(),
        )
        || attempt.client_nonce.len() != 43
        || attempt.client_proof.len() != 43
    {
        bail!("saved claim attempt does not match this output path");
    }
    let nonce = decode_claim_attempt_nonce(attempt)?;
    let stored_proof = Zeroizing::new(
        URL_SAFE_NO_PAD
            .decode(&attempt.client_proof)
            .context("saved claim attempt has an invalid proof")?,
    );
    if stored_proof.len() != 32
        || !constant_time_equal(&stored_proof, claim_attempt_proof(code, &nonce).as_ref())
    {
        bail!("saved claim attempt does not match this setup code");
    }
    Ok(())
}

fn load_claim_attempt(path: &Path) -> Result<Option<ClaimAttemptJournal>> {
    match fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
    }
    let bytes = Zeroizing::new(read_bounded_regular_file(
        path,
        MAX_CLAIM_ATTEMPT_BYTES,
        true,
    )?);
    let attempt: ClaimAttemptJournal =
        serde_json::from_slice(&bytes).context("saved claim attempt is invalid")?;
    Ok(Some(attempt))
}

fn create_claim_attempt(path: &Path, output_dir: &Path, code: &str) -> Result<ClaimAttemptJournal> {
    let mut nonce = Zeroizing::new([0_u8; CLAIM_NONCE_BYTES]);
    OsRng.fill_bytes(nonce.as_mut());
    let proof = claim_attempt_proof(code, nonce.as_ref());
    let attempt = ClaimAttemptJournal {
        schema_version: CLAIM_ATTEMPT_SCHEMA_VERSION,
        output_path_digest: claim_output_path_digest(output_dir),
        client_nonce: URL_SAFE_NO_PAD.encode(nonce.as_ref()),
        client_proof: URL_SAFE_NO_PAD.encode(proof.as_ref()),
    };
    let bytes = Zeroizing::new(serde_json::to_vec_pretty(&attempt)?);
    if bytes.len() as u64 > MAX_CLAIM_ATTEMPT_BYTES {
        bail!("claim attempt journal exceeds the safe size limit");
    }
    let parent = path
        .parent()
        .context("claim attempt path must have an existing parent")?;
    let temporary = write_private_temporary_file(parent, &bytes)?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| error.error)
        .with_context(|| format!("publish claim attempt journal {}", path.display()))?;
    sync_directory(parent)?;
    Ok(attempt)
}

fn remove_claim_attempt(path: &Path, expected: &ClaimAttemptJournal) -> Result<()> {
    let current = load_claim_attempt(path)?.context("saved claim attempt disappeared")?;
    if &current != expected {
        bail!("saved claim attempt changed before completion");
    }
    fs::remove_file(path).with_context(|| format!("remove claim attempt {}", path.display()))?;
    sync_directory(
        path.parent()
            .context("claim attempt path must have an existing parent")?,
    )
}

fn reconcile_published_claim_credentials(https_url: &str, output_dir: &Path) -> Result<()> {
    reconcile_published_claim_credentials_with_curl(Path::new("curl"), https_url, output_dir)
}

fn reconcile_published_claim_credentials_with_curl(
    curl_binary: &Path,
    https_url: &str,
    output_dir: &Path,
) -> Result<()> {
    let metadata = fs::symlink_metadata(output_dir)
        .with_context(|| format!("inspect claim output {}", output_dir.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "claim output must be a real directory: {}",
            output_dir.display()
        );
    }
    require_private_directory_permissions(&metadata, output_dir)?;
    let ca_path = output_dir.join("root.crt");
    let ca = read_private_text(&ca_path, MAX_CLAIM_CA_PEM_BYTES as u64, "claim CA")?;
    validate_exact_ca_certificate_pem(&ca)
        .context("saved claim CA must contain exactly one valid X.509 CA certificate")?;
    let token = read_private_text(&output_dir.join("local-api-token"), 512, "local API token")?;
    if token.len() < 32
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b'"' && byte != b'\\')
    {
        bail!("saved claim token is invalid");
    }
    curl_verify_ca_hostname_and_token_with_curl(
        curl_binary,
        &format!("{https_url}/api/v1/backups"),
        &ca_path,
        token.as_bytes(),
        output_dir,
    )
}

fn normalise_https_origin(value: &str) -> Result<String> {
    let origin = value.trim().trim_end_matches('/');
    let authority = origin
        .strip_prefix("https://")
        .context("--https-url must start with https://")?;
    if authority.is_empty()
        || authority.contains(['/', '?', '#', '@', '\\'])
        || authority.bytes().any(|byte| byte.is_ascii_whitespace())
        || !authority
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':'))
    {
        bail!("--https-url must be one explicit HTTPS hostname with an optional port");
    }
    let hostname = authority
        .rsplit_once(':')
        .map_or(authority, |(host, port)| {
            if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
                ""
            } else {
                host
            }
        });
    if hostname.is_empty() || hostname.starts_with('.') || hostname.ends_with('.') {
        bail!("--https-url must contain a valid hostname");
    }
    Ok(format!("https://{authority}"))
}

const MAX_CLAIM_RESPONSE_BYTES: usize = 256 * 1_024;
const CLAIM_CURL_TIMEOUT: Duration = Duration::from_secs(35);
const CLAIM_CURL_POLL_INTERVAL: Duration = Duration::from_millis(5);
const CLAIM_RESPONSE_READ_BUFFER_BYTES: usize = 8 * 1_024;

#[cfg(test)]
static CLAIM_RESPONSE_PEAK_BYTES: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

fn curl_bootstrap_claim(url: &str, request: &serde_json::Value) -> Result<Zeroizing<Vec<u8>>> {
    curl_bootstrap_claim_with_curl(Path::new("curl"), url, request, CLAIM_CURL_TIMEOUT)
}

fn curl_bootstrap_claim_with_curl(
    curl_binary: &Path,
    url: &str,
    request: &serde_json::Value,
    timeout: Duration,
) -> Result<Zeroizing<Vec<u8>>> {
    let request = Zeroizing::new(serde_json::to_vec(request)?);
    let maximum = MAX_CLAIM_RESPONSE_BYTES.to_string();
    let mut child = ProcessCommand::new(curl_binary)
        .args([
            "--proto",
            "=https",
            "--connect-timeout",
            "10",
            "--max-time",
            "30",
            "--max-filesize",
            &maximum,
            "--fail",
            "--silent",
            "--show-error",
            "--insecure",
            "--request",
            "POST",
            "--header",
            "Content-Type: application/json",
            "--data-binary",
            "@-",
            url,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // Curl's diagnostics can contain an attacker-controlled URL or body
        // fragment. Keep stderr out of memory and out of operator logs.
        .stderr(Stdio::null())
        .spawn()
        .context("trusted claim requires the curl command")?;
    let result = (|| {
        child
            .stdin
            .take()
            .context("open claim request input")?
            .write_all(&request)
            .context("send claim proof")?;
        let stdout = child.stdout.take().context("open claim response stream")?;
        collect_bounded_claim_stdout(&mut child, stdout, MAX_CLAIM_RESPONSE_BYTES, timeout)
    })();
    if result.is_err() {
        terminate_and_reap(&mut child);
    }
    let output = result?;
    if !output.status.success() {
        bail!(
            "claim request failed; rerun this command with the same setup-code and output files so the saved exact request can be recovered"
        )
    }
    Ok(output.stdout)
}

struct BoundedClaimOutput {
    status: ExitStatus,
    stdout: Zeroizing<Vec<u8>>,
}

#[cfg(unix)]
fn collect_bounded_claim_stdout(
    child: &mut Child,
    mut stdout: ChildStdout,
    maximum: usize,
    timeout: Duration,
) -> Result<BoundedClaimOutput> {
    use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};

    let flags = fcntl_getfl(&stdout).context("inspect claim response stream")?;
    fcntl_setfl(&stdout, flags | OFlags::NONBLOCK).context("bound claim response stream reads")?;
    let started = Instant::now();
    let mut response = Zeroizing::new(Vec::with_capacity(maximum));
    let mut buffer = [0_u8; CLAIM_RESPONSE_READ_BUFFER_BYTES];
    let mut stdout_closed = false;
    loop {
        if !stdout_closed {
            match stdout.read(&mut buffer) {
                Ok(0) => stdout_closed = true,
                Ok(read) => {
                    if read > maximum.saturating_sub(response.len()) {
                        terminate_and_reap(child);
                        bail!("claim response exceeds the safe size limit");
                    }
                    response.extend_from_slice(&buffer[..read]);
                    #[cfg(test)]
                    CLAIM_RESPONSE_PEAK_BYTES
                        .store(response.len(), std::sync::atomic::Ordering::Relaxed);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error).context("read bounded claim response"),
            }
        }
        if stdout_closed {
            if let Some(status) = child.try_wait().context("inspect claim process")? {
                return Ok(BoundedClaimOutput {
                    status,
                    stdout: response,
                });
            }
        } else {
            // Poll process state while continuing to drain to EOF. Even if a
            // foreign descendant inherited curl's stdout, the parent deadline
            // still terminates our wait.
            let _ = child.try_wait().context("inspect claim process")?;
        }
        if started.elapsed() >= timeout {
            terminate_and_reap(child);
            bail!("claim request exceeded the local safety timeout");
        }
        std::thread::sleep(CLAIM_CURL_POLL_INTERVAL.min(timeout));
    }
}

#[cfg(not(unix))]
fn collect_bounded_claim_stdout(
    child: &mut Child,
    mut stdout: ChildStdout,
    maximum: usize,
    timeout: Duration,
) -> Result<BoundedClaimOutput> {
    use std::sync::mpsc;

    let started = Instant::now();
    let (sender, receiver) = mpsc::sync_channel(1);
    let reader = std::thread::spawn(move || {
        let mut response = Zeroizing::new(Vec::with_capacity(maximum));
        let mut buffer = [0_u8; CLAIM_RESPONSE_READ_BUFFER_BYTES];
        let result = loop {
            match stdout.read(&mut buffer) {
                Ok(0) => break Ok((response, false)),
                Ok(read) if read > maximum.saturating_sub(response.len()) => {
                    break Ok((response, true));
                }
                Ok(read) => response.extend_from_slice(&buffer[..read]),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => break Err(error),
            }
        };
        let _ = sender.send(result);
    });
    let (response, exceeded) = match receiver.recv_timeout(timeout) {
        Ok(result) => result.context("read bounded claim response")?,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            terminate_and_reap(child);
            let _ = reader.join();
            bail!("claim request exceeded the local safety timeout");
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            terminate_and_reap(child);
            let _ = reader.join();
            bail!("claim response reader stopped unexpectedly");
        }
    };
    if exceeded {
        terminate_and_reap(child);
        let _ = reader.join();
        bail!("claim response exceeds the safe size limit");
    }
    #[cfg(test)]
    CLAIM_RESPONSE_PEAK_BYTES.store(response.len(), std::sync::atomic::Ordering::Relaxed);
    let status = loop {
        if let Some(status) = child.try_wait().context("inspect claim process")? {
            break status;
        }
        if started.elapsed() >= timeout {
            terminate_and_reap(child);
            let _ = reader.join();
            bail!("claim request exceeded the local safety timeout");
        }
        std::thread::sleep(CLAIM_CURL_POLL_INTERVAL.min(timeout));
    };
    let _ = reader.join();
    Ok(BoundedClaimOutput {
        status,
        stdout: response,
    })
}

fn terminate_and_reap(child: &mut Child) {
    if matches!(child.try_wait(), Ok(None)) {
        let _ = child.kill();
    }
    let _ = child.wait();
}

fn parse_claim_response(response: &[u8]) -> Result<ClaimResponse> {
    if response.len() > MAX_CLAIM_RESPONSE_BYTES {
        bail!("claim response exceeds the safe size limit");
    }
    serde_json::from_slice(response).context("claim server returned an invalid response")
}

fn verify_claim_ca(pem: &str, expected_fingerprint: &str) -> Result<VerifiedClaimCa> {
    if pem.len() > MAX_CLAIM_CA_PEM_BYTES {
        bail!("claim CA certificate exceeds the safe size limit");
    }
    let der = validate_exact_ca_certificate_pem(pem)
        .context("claim response must contain exactly one valid PEM X.509 CA certificate")?;
    let digest: [u8; 32] = Sha256::digest(&der).into();
    let actual = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if expected_fingerprint.len() != 64
        || !expected_fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || !actual.eq_ignore_ascii_case(expected_fingerprint)
    {
        bail!("claim CA fingerprint does not match the delivered certificate");
    }
    Ok(VerifiedClaimCa {
        canonical_pem: canonical_certificate_pem(&der),
        digest,
    })
}

fn canonical_certificate_pem(der: &[u8]) -> String {
    const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
    const END: &str = "-----END CERTIFICATE-----";
    let encoded = STANDARD.encode(der);
    let mut pem =
        String::with_capacity(BEGIN.len() + END.len() + encoded.len() + (encoded.len() / 64) + 4);
    pem.push_str(BEGIN);
    pem.push('\n');
    for chunk in encoded.as_bytes().chunks(64) {
        // Base64 output is ASCII, so this slice is always valid UTF-8.
        pem.push_str(std::str::from_utf8(chunk).expect("base64 is UTF-8"));
        pem.push('\n');
    }
    pem.push_str(END);
    pem.push('\n');
    pem
}

fn curl_verify_ca_hostname_and_token(
    url: &str,
    ca_path: &Path,
    token: &[u8],
    temporary_directory: &Path,
) -> Result<()> {
    curl_verify_ca_hostname_and_token_with_curl(
        Path::new("curl"),
        url,
        ca_path,
        token,
        temporary_directory,
    )
}

fn curl_verify_ca_hostname_and_token_with_curl(
    curl_binary: &Path,
    url: &str,
    ca_path: &Path,
    token: &[u8],
    temporary_directory: &Path,
) -> Result<()> {
    let token = std::str::from_utf8(token).context("claim token is not valid text")?;
    let mut config = NamedTempFile::new_in(temporary_directory)
        .context("create private HTTPS verification configuration")?;
    set_private_permissions(config.path())?;
    writeln!(
        config,
        "url = \"{}\"\ncacert = \"{}\"\nheader = \"Authorization: Bearer {}\"\nfail\nsilent\nshow-error\noutput = \"/dev/null\"",
        curl_config_value(url)?,
        curl_config_value(&ca_path.display().to_string())?,
        curl_config_value(token)?,
    )
    .context("write private HTTPS verification configuration")?;
    config
        .as_file()
        .sync_all()
        .context("sync private HTTPS verification configuration")?;
    let status = ProcessCommand::new(curl_binary)
        .arg("--proto")
        .arg("=https")
        .arg("--connect-timeout")
        .arg("10")
        .arg("--max-time")
        .arg("30")
        .arg("--config")
        .arg(config.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .context("verify claimed HTTPS endpoint")?;
    if !status.success() {
        bail!(
            "returned CA, hostname, or local API token could not be verified; credentials were not saved"
        );
    }
    Ok(())
}

fn curl_config_value(value: &str) -> Result<String> {
    if value
        .chars()
        .any(|character| character.is_control() || character == '\0')
    {
        bail!("claim credential contains an unsafe control character");
    }
    Ok(value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn prepare_claim_output_directory(path: &Path) -> Result<TempDir> {
    let path = validated_claim_output_path(path)?;
    match fs::symlink_metadata(&path) {
        Ok(_) => bail!(
            "claim output directory already exists; choose a new empty path so credentials are never overwritten: {}",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
    }
    let parent = path
        .parent()
        .context("claim output directory must have an existing parent directory")?;
    let directory = tempfile::Builder::new()
        .prefix(".covalent-claim-")
        .tempdir_in(parent)
        .context("create private claim staging directory")?;
    set_private_directory_permissions(directory.path())?;
    Ok(directory)
}

fn write_private_temporary_file(directory: &Path, contents: &[u8]) -> Result<NamedTempFile> {
    let mut file =
        NamedTempFile::new_in(directory).context("create private temporary credential")?;
    set_private_permissions(file.path())?;
    file.write_all(contents)
        .context("write private temporary credential")?;
    file.as_file()
        .sync_all()
        .context("sync private temporary credential")?;
    Ok(file)
}

fn persist_claim_credentials(directory: &Path, ca: NamedTempFile, token: &[u8]) -> Result<()> {
    let ca_target = directory.join("root.crt");
    let token_target = directory.join("local-api-token");
    let ca = ca
        .persist_noclobber(&ca_target)
        .map_err(|error| error.error)
        .context("save exact CA without replacing an existing file")?;
    let token_file = write_private_temporary_file(directory, token)?;
    if let Err(error) = token_file.persist_noclobber(&token_target) {
        let _ = fs::remove_file(&ca_target);
        return Err(error.error).context("save local API token without replacing an existing file");
    }
    ca.sync_all().context("sync exact CA")?;
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    std::fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("sync credential directory {}", path.display()))
}

#[cfg(not(unix))]
fn sync_directory(_: &Path) -> Result<()> {
    Ok(())
}

fn read_owner_key_file(path: &Path) -> Result<Zeroizing<String>> {
    read_private_text(path, 512, "KEK")
}

fn read_setup_code(path: &Path) -> Result<Zeroizing<String>> {
    let code = read_private_text(path, 128, "setup code")?;
    normalise_claim_code(&code).context("setup code must be ten valid characters")
}

fn read_private_text(path: &Path, maximum: u64, label: &str) -> Result<Zeroizing<String>> {
    let contents = read_bounded_regular_file(path, maximum, true)
        .with_context(|| format!("read private {label} file {}", path.display()))?;
    let text = std::str::from_utf8(&contents)
        .with_context(|| format!("{label} file {} is not UTF-8", path.display()))?;
    Ok(Zeroizing::new(text.to_owned()))
}

#[cfg(unix)]
fn require_private_directory_permissions(metadata: &fs::Metadata, path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    if metadata.mode() & 0o077 != 0 {
        bail!(
            "claim output parent {} must not be accessible to group or others",
            path.display()
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn require_private_directory_permissions(_: &fs::Metadata, _: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_private_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("set private permissions on {}", path.display()))
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("set private permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn set_private_permissions(_: &Path) -> Result<()> {
    Ok(())
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_: &Path) -> Result<()> {
    Ok(())
}

fn parse_backup_id(value: &str) -> Result<BackupId> {
    BackupId::from_str(value).with_context(|| format!("invalid backup ID {value}"))
}

fn parse_device_id(value: &str) -> Result<DeviceId> {
    DeviceId::from_str(value).with_context(|| format!("invalid device ID {value}"))
}

fn read_bounded(path: &Path, maximum: u64) -> Result<Vec<u8>> {
    read_bounded_regular_file(path, maximum, false)
}

fn read_bounded_regular_file(path: &Path, maximum: u64, owner_only: bool) -> Result<Vec<u8>> {
    #[cfg(unix)]
    let (file, length) = {
        use rustix::fs::{FileType, Mode, OFlags, fstat, open};

        let descriptor = open(
            path,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .with_context(|| format!("open {} without following links", path.display()))?;
        let stat = fstat(&descriptor)
            .with_context(|| format!("inspect open file handle for {}", path.display()))?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
            || stat.st_size < 0
            || stat.st_size as u64 > maximum
        {
            bail!(
                "{} must be a regular file no larger than {} bytes",
                path.display(),
                maximum
            );
        }
        if owner_only && stat.st_mode & 0o077 != 0 {
            bail!(
                "{} must not be readable or writable by group or others",
                path.display()
            );
        }
        (File::from(descriptor), stat.st_size as u64)
    };
    #[cfg(not(unix))]
    let (file, length) = {
        let _ = owner_only;
        let metadata =
            fs::symlink_metadata(path).with_context(|| format!("inspect {}", path.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > maximum {
            bail!(
                "{} must be a regular file no larger than {} bytes",
                path.display(),
                maximum
            );
        }
        let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
        (file, metadata.len())
    };

    let mut bytes = Vec::with_capacity(length as usize);
    file.take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)
        .with_context(|| format!("read {}", path.display()))?;
    if bytes.len() as u64 > maximum {
        bail!("{} exceeds the {} byte limit", path.display(), maximum);
    }
    Ok(bytes)
}

fn write_or_print(path: Option<&Path>, bytes: &[u8]) -> Result<()> {
    if let Some(path) = path {
        fs::write(path, bytes).with_context(|| format!("write {}", path.display()))?;
    } else {
        println!("{}", String::from_utf8_lossy(bytes));
    }
    Ok(())
}

fn print_json(value: &impl Serialize) -> Result<()> {
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    write_json(&mut output, value)
}

fn write_json(output: &mut impl Write, value: &impl Serialize) -> Result<()> {
    serde_json::to_writer_pretty(&mut *output, value)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

fn write_backup_report_and_acknowledge(
    engine: &Engine,
    job_id: &str,
    report: &BackupReport,
    output: &mut impl Write,
) -> Result<()> {
    write_json(output, report)?;
    engine.acknowledge_backup_result(job_id)?;
    Ok(())
}

fn default_backup_job_id(
    source: &Path,
    name: &str,
    backup_id: Option<&str>,
    snapshot_id: &str,
    providers: &[String],
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"covalent/cli-backup-job/v1");
    for field in [
        source.as_os_str().as_encoded_bytes(),
        name.as_bytes(),
        backup_id.unwrap_or_default().as_bytes(),
        snapshot_id.as_bytes(),
    ] {
        digest.update((field.len() as u64).to_be_bytes());
        digest.update(field);
    }
    digest.update((providers.len() as u64).to_be_bytes());
    for provider in providers {
        digest.update((provider.len() as u64).to_be_bytes());
        digest.update(provider.as_bytes());
    }
    format!("cli-backup-{}", URL_SAFE_NO_PAD.encode(digest.finalize()))
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    static CLAIM_CURL_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct FailingOutput;

    impl Write for FailingOutput {
        fn write(&mut self, _buffer: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("forced output failure"))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::other("forced output failure"))
        }
    }

    fn test_engine(directory: &TempDir) -> Engine {
        Engine::open(
            EngineOptions::new(directory.path()).with_key_protector(Arc::new(
                StaticKeyProtector::new(1, [0x63; 32]).expect("test protector"),
            )),
        )
        .expect("test engine")
    }

    fn backup_report(backup_id: BackupId, result: covalent_core::BackupResult) -> BackupReport {
        BackupReport {
            backup_id: backup_id.to_string(),
            snapshot_id: result.manifest.snapshot_id,
            entries: result.manifest.entries.len(),
            bytes_read: result.progress.bytes_read,
            chunks_stored: result.progress.chunks_stored,
            chunks_deduplicated: result.progress.chunks_deduplicated,
            selected_providers: result.manifest.replica_intent.selected_providers.len(),
            provider_failures: result.replication.failures.len(),
        }
    }

    fn fingerprint(der: &[u8]) -> String {
        Sha256::digest(der)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    #[test]
    fn default_backup_job_ids_are_stable_and_distinguish_requests() {
        let source = Path::new("Documents");
        let first = default_backup_job_id(source, "Files", None, "snapshot-1", &[]);
        assert_eq!(
            first,
            default_backup_job_id(source, "Files", None, "snapshot-1", &[])
        );
        assert_ne!(
            first,
            default_backup_job_id(source, "Files", None, "snapshot-2", &[])
        );
    }

    #[test]
    fn acknowledged_cli_output_keeps_more_than_eight_distinct_backups_live() {
        let data = TempDir::new().expect("data");
        let source = TempDir::new().expect("source");
        fs::write(source.path().join("file.txt"), b"cli output").expect("source file");
        let engine = test_engine(&data);

        for index in 0..=covalent_core::MAX_UNACKNOWLEDGED_BACKUP_RESULTS {
            let backup_id = BackupId::new();
            let job_id = format!("cli-distinct-{index}");
            let options = BackupOptions::new(backup_id, format!("snapshot-{index}"), &job_id);
            let result = engine
                .backup(source.path(), &options, &JobControl::new(), |_| {})
                .expect("backup");
            let mut output = Vec::new();
            write_backup_report_and_acknowledge(
                &engine,
                &job_id,
                &backup_report(backup_id, result),
                &mut output,
            )
            .expect("durable output then acknowledgement");
            assert!(!output.is_empty());
            assert_eq!(
                engine
                    .unacknowledged_backup_id(&job_id)
                    .expect("receipt state"),
                None
            );
        }
    }

    #[test]
    fn cli_output_failure_retains_byte_identical_result_for_retry() {
        let data = TempDir::new().expect("data");
        let source = TempDir::new().expect("source");
        fs::write(source.path().join("file.txt"), b"retryable cli output").expect("source file");
        let engine = test_engine(&data);
        let backup_id = BackupId::new();
        let job_id = "cli-output-failure";
        let options = BackupOptions::new(backup_id, "snapshot-1", job_id);
        let first = engine
            .backup(source.path(), &options, &JobControl::new(), |_| {})
            .expect("backup");
        let mut failed = FailingOutput;
        assert!(
            write_backup_report_and_acknowledge(
                &engine,
                job_id,
                &backup_report(backup_id, first.clone()),
                &mut failed,
            )
            .is_err()
        );
        assert_eq!(
            engine
                .unacknowledged_backup_id(job_id)
                .expect("retained result"),
            Some(backup_id)
        );
        let retry = engine
            .backup(source.path(), &options, &JobControl::new(), |_| {})
            .expect("retry");
        assert_eq!(retry, first);

        let mut output = Vec::new();
        write_backup_report_and_acknowledge(
            &engine,
            job_id,
            &backup_report(backup_id, retry),
            &mut output,
        )
        .expect("successful retry output");
        assert_eq!(
            engine
                .unacknowledged_backup_id(job_id)
                .expect("acknowledged result"),
            None
        );
    }

    fn certificate_der(is_ca: bool) -> Vec<u8> {
        let mut parameters =
            rcgen::CertificateParams::new(Vec::<String>::new()).expect("certificate parameters");
        if is_ca {
            parameters.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        }
        let key = rcgen::KeyPair::generate().expect("certificate key");
        parameters
            .self_signed(&key)
            .expect("self-signed certificate")
            .der()
            .to_vec()
    }

    #[test]
    fn claim_ca_accepts_one_certificate_and_persists_only_canonical_bytes() {
        let der = certificate_der(true);
        let input = format!(" \n\t{}\r\n", canonical_certificate_pem(&der));
        let verified =
            verify_claim_ca(&input, &fingerprint(&der)).expect("valid single certificate");

        let digest: [u8; 32] = Sha256::digest(&der).into();
        assert_eq!(verified.digest, digest);
        assert_eq!(verified.canonical_pem, canonical_certificate_pem(&der));
    }

    #[test]
    fn claim_ca_rejects_an_appended_second_certificate() {
        let first_der = certificate_der(true);
        let second_der = certificate_der(true);
        let first = canonical_certificate_pem(&first_der);
        let second = canonical_certificate_pem(&second_der);
        let error = verify_claim_ca(&format!("{first}{second}"), &fingerprint(&first_der))
            .expect_err("a trust bundle must not be accepted as one CA");

        assert!(error.to_string().contains("exactly one valid PEM"));
    }

    #[test]
    fn claim_ca_rejects_garbage_before_or_after_the_certificate() {
        let der = certificate_der(true);
        let pem = canonical_certificate_pem(&der);
        let fingerprint = fingerprint(&der);

        assert!(verify_claim_ca(&format!("garbage{pem}"), &fingerprint).is_err());
        assert!(verify_claim_ca(&format!("{pem}garbage"), &fingerprint).is_err());
    }

    #[test]
    fn claim_ca_rejects_a_wrong_fingerprint() {
        let pem = canonical_certificate_pem(&certificate_der(true));

        assert!(verify_claim_ca(&pem, &"00".repeat(32)).is_err());
    }

    #[test]
    fn claim_ca_rejects_a_real_x509_leaf_certificate() {
        let der = certificate_der(false);
        let pem = canonical_certificate_pem(&der);

        assert!(verify_claim_ca(&pem, &fingerprint(&der)).is_err());
    }

    #[test]
    fn claim_ca_deserialization_refuses_an_oversized_string() {
        let oversized = "A".repeat(MAX_CLAIM_CA_PEM_BYTES + 1);
        let encoded = serde_json::to_string(&oversized).expect("JSON");

        assert!(serde_json::from_str::<ClaimCaPem>(&encoded).is_err());
    }

    #[test]
    fn claim_response_refuses_an_oversized_payload_before_parsing() {
        let oversized = vec![b' '; MAX_CLAIM_RESPONSE_BYTES + 1];

        assert!(parse_claim_response(&oversized).is_err());
    }

    #[cfg(unix)]
    fn write_fake_streaming_curl(directory: &Path, loop_body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;

        let binary = directory.join("curl-stream");
        let pid_path = directory
            .join("curl.pid")
            .display()
            .to_string()
            .replace('\'', "'\\''");
        fs::write(
            &binary,
            format!(
                "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$$\" > '{pid_path}'\nwhile :; do\n  {loop_body}\ndone\n"
            ),
        )
        .expect("write fake streaming curl");
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700))
            .expect("make fake curl executable");
        binary
    }

    #[cfg(unix)]
    fn assert_fake_curl_reaped(directory: &Path) {
        let process_id = fs::read_to_string(directory.join("curl.pid"))
            .expect("fake curl process id")
            .trim()
            .to_owned();
        let status = ProcessCommand::new("kill")
            .args(["-0", &process_id])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("probe fake curl process");
        assert!(
            !status.success(),
            "oversized or stalled curl must be reaped"
        );
    }

    #[cfg(unix)]
    #[test]
    fn claim_transport_kills_an_infinite_response_at_the_memory_cap() {
        let _serial = CLAIM_CURL_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let directory = TempDir::new().expect("fake curl directory");
        let curl = write_fake_streaming_curl(
            directory.path(),
            "printf '0123456789abcdef0123456789abcdef'",
        );
        CLAIM_RESPONSE_PEAK_BYTES.store(0, std::sync::atomic::Ordering::Relaxed);

        let error = curl_bootstrap_claim_with_curl(
            &curl,
            "https://claim.invalid/api/v1/claim",
            &serde_json::json!({"clientNonce": "nonce", "clientProof": "proof"}),
            Duration::from_secs(5),
        )
        .expect_err("an infinite response must be terminated at the cap");

        assert!(error.to_string().contains("safe size limit"));
        assert!(
            CLAIM_RESPONSE_PEAK_BYTES.load(std::sync::atomic::Ordering::Relaxed)
                <= MAX_CLAIM_RESPONSE_BYTES,
            "the collector must never retain a byte beyond the protocol cap"
        );
        assert_fake_curl_reaped(directory.path());
    }

    #[cfg(unix)]
    #[test]
    fn claim_transport_kills_and_reaps_a_silent_process_at_its_parent_deadline() {
        let _serial = CLAIM_CURL_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let directory = TempDir::new().expect("fake curl directory");
        let curl = write_fake_streaming_curl(directory.path(), ":");
        let started = Instant::now();

        let error = curl_bootstrap_claim_with_curl(
            &curl,
            "https://claim.invalid/api/v1/claim",
            &serde_json::json!({"clientNonce": "nonce", "clientProof": "proof"}),
            Duration::from_secs(1),
        )
        .expect_err("a fake curl that ignores --max-time must be terminated");

        assert!(error.to_string().contains("local safety timeout"));
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_fake_curl_reaped(directory.path());
    }

    #[cfg(unix)]
    #[test]
    fn private_cli_text_reader_refuses_symlinks_and_broad_permissions() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let directory = TempDir::new().expect("directory");
        let target = directory.path().join("secret.txt");
        let link = directory.path().join("secret-link.txt");
        let contents = "private-cli-secret-with-at-least-thirty-two-bytes\n";
        fs::write(&target, contents).expect("secret");
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600))
            .expect("private permissions");
        symlink(&target, &link).expect("symlink");
        assert!(read_private_text(&link, 128, "test secret").is_err());
        assert_eq!(
            fs::read_to_string(&target).expect("target intact"),
            contents
        );

        fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).expect("broad permissions");
        assert!(read_private_text(&target, 128, "test secret").is_err());
    }

    #[cfg(unix)]
    fn write_private_setup_code(directory: &Path, name: &str, contents: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;

        let path = directory.join(name);
        fs::write(&path, contents).expect("write setup code fixture");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .expect("make setup code fixture owner-only");
        path
    }

    #[cfg(unix)]
    #[test]
    fn setup_code_files_accept_common_editor_line_endings_and_ascii_spacing() {
        let directory = TempDir::new().expect("private setup-code directory");
        for (name, contents) in [
            ("lf", "01234-56789\n"),
            ("crlf", "01234-56789\r\n"),
            ("editor-spacing", " \t\r\n01234-\r\n56789 \t\r\n"),
        ] {
            let path = write_private_setup_code(directory.path(), name, contents);
            assert_eq!(
                read_setup_code(&path)
                    .expect("valid editor-style setup code")
                    .as_str(),
                "0123456789"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn setup_code_files_reject_non_protocol_whitespace_garbage_and_oversize_input() {
        let directory = TempDir::new().expect("private setup-code directory");
        for (name, contents) in [
            ("vertical-tab", "01234\u{000b}56789"),
            ("form-feed", "01234\u{000c}56789"),
            ("unicode-space", "01234\u{00a0}56789"),
            ("unicode-letter", "01234-é56789"),
            ("garbage", "01234-56789@"),
        ] {
            let path = write_private_setup_code(directory.path(), name, contents);
            assert!(read_setup_code(&path).is_err(), "{name} must fail closed");
        }

        let parser_oversize = write_private_setup_code(
            directory.path(),
            "parser-oversize",
            &format!("01234-56789{}", " ".repeat(54)),
        );
        assert!(
            read_setup_code(&parser_oversize).is_err(),
            "more than 64 parser bytes must fail closed"
        );

        let reader_oversize =
            write_private_setup_code(directory.path(), "reader-oversize", &"0".repeat(129));
        assert!(
            read_setup_code(&reader_oversize).is_err(),
            "more than 128 file bytes must fail before parsing"
        );
    }

    #[cfg(unix)]
    #[test]
    fn claim_attempt_journal_is_private_exact_and_contains_no_setup_code_or_token() {
        use std::os::unix::fs::MetadataExt as _;

        let parent = TempDir::new().expect("private claim parent");
        set_private_directory_permissions(parent.path()).expect("private claim parent permissions");
        let output =
            validated_claim_output_path(&parent.path().join("claimed")).expect("validated output");
        let journal_path = claim_attempt_journal_path(&output).expect("journal path");
        let code = "0123456789";
        let attempt =
            create_claim_attempt(&journal_path, &output, code).expect("durable claim attempt");
        let bytes = fs::read(&journal_path).expect("journal bytes");

        assert_eq!(
            fs::metadata(&journal_path).expect("metadata").mode() & 0o777,
            0o600
        );
        assert!(
            !bytes
                .windows(code.len())
                .any(|window| window == code.as_bytes())
        );
        assert!(!String::from_utf8_lossy(&bytes).contains("local-api-token"));
        assert_eq!(
            load_claim_attempt(&journal_path)
                .expect("load journal")
                .expect("journal"),
            attempt
        );
        validate_claim_attempt(&attempt, &output, code).expect("exact attempt");
        assert!(
            validate_claim_attempt(&attempt, &parent.path().join("other"), code).is_err(),
            "the journal must be bound to one canonical output path"
        );
        assert!(
            validate_claim_attempt(&attempt, &output, "012345678A").is_err(),
            "a different setup code must not reuse the saved proof"
        );
    }

    #[cfg(unix)]
    #[test]
    fn claim_attempt_journal_refuses_symlinks_broad_permissions_and_oversize_files() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let parent = TempDir::new().expect("private claim parent");
        set_private_directory_permissions(parent.path()).expect("private claim parent permissions");
        let output =
            validated_claim_output_path(&parent.path().join("claimed")).expect("validated output");
        let journal_path = claim_attempt_journal_path(&output).expect("journal path");
        create_claim_attempt(&journal_path, &output, "0123456789").expect("claim attempt");

        fs::set_permissions(&journal_path, fs::Permissions::from_mode(0o640))
            .expect("broaden permissions");
        assert!(load_claim_attempt(&journal_path).is_err());
        fs::set_permissions(&journal_path, fs::Permissions::from_mode(0o600))
            .expect("restore permissions");

        let target = parent.path().join("journal-target");
        fs::rename(&journal_path, &target).expect("move journal target");
        symlink(&target, &journal_path).expect("journal symlink");
        assert!(load_claim_attempt(&journal_path).is_err());
        fs::remove_file(&journal_path).expect("remove symlink");

        fs::write(
            &journal_path,
            vec![b' '; MAX_CLAIM_ATTEMPT_BYTES as usize + 1],
        )
        .expect("oversized journal");
        fs::set_permissions(&journal_path, fs::Permissions::from_mode(0o600))
            .expect("private oversized journal");
        assert!(load_claim_attempt(&journal_path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn published_credentials_are_verified_before_a_crash_left_journal_is_pruned() {
        use std::os::unix::fs::PermissionsExt as _;

        const ORIGIN: &str = "https://atlas.example-tailnet.ts.net:8443";
        const TOKEN: &str = "test-local-api-token-with-at-least-thirty-two-bytes";
        let parent = TempDir::new().expect("private claim parent");
        set_private_directory_permissions(parent.path()).expect("private claim parent permissions");
        let output =
            validated_claim_output_path(&parent.path().join("claimed")).expect("validated output");
        let journal_path = claim_attempt_journal_path(&output).expect("journal path");
        let attempt = create_claim_attempt(&journal_path, &output, "0123456789")
            .expect("durable claim attempt");

        fs::create_dir(&output).expect("published credential directory");
        fs::set_permissions(&output, fs::Permissions::from_mode(0o700))
            .expect("private credential directory");
        let ca_path = output.join("root.crt");
        fs::write(&ca_path, canonical_certificate_pem(&certificate_der(true)))
            .expect("published CA");
        fs::write(output.join("local-api-token"), TOKEN).expect("published token");
        fs::set_permissions(&ca_path, fs::Permissions::from_mode(0o600)).expect("private CA");
        fs::set_permissions(
            output.join("local-api-token"),
            fs::Permissions::from_mode(0o600),
        )
        .expect("private token");
        let verifier = write_fake_claim_verifier(
            parent.path(),
            &format!("{ORIGIN}/api/v1/backups"),
            &ca_path,
            TOKEN,
        );

        reconcile_published_claim_credentials_with_curl(&verifier, ORIGIN, &output)
            .expect("verify already-published credentials after restart");
        assert!(
            journal_path.exists(),
            "verification alone must not prune recovery state"
        );
        remove_claim_attempt(&journal_path, &attempt)
            .expect("prune journal only after credential verification");
        assert!(!journal_path.exists());
    }

    #[test]
    fn claim_refuses_to_replace_an_existing_output_directory() {
        let parent = TempDir::new().expect("private parent");
        set_private_directory_permissions(parent.path()).expect("private parent permissions");
        let output = parent.path().join("existing-claim");
        fs::create_dir(&output).expect("existing output directory");

        let error = prepare_claim_output_directory(&output)
            .expect_err("claim credentials must never overwrite an output directory");
        assert!(error.to_string().contains("already exists"));
    }

    #[cfg(unix)]
    fn write_fake_claim_verifier(
        directory: &Path,
        expected_url: &str,
        expected_ca: &Path,
        expected_token: &str,
    ) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;

        let expected_config = format!(
            "url = \"{expected_url}\"\ncacert = \"{}\"\nheader = \"Authorization: Bearer {expected_token}\"\nfail\nsilent\nshow-error\noutput = \"/dev/null\"\n",
            expected_ca.display()
        );
        fs::write(directory.join("expected-curl-config"), expected_config)
            .expect("write expected curl configuration");
        let binary = directory.join("curl");
        fs::write(
            &binary,
            "#!/bin/sh\nset -eu\n[ \"$#\" -eq 8 ]\n[ \"$1\" = --proto ]\n[ \"$2\" = =https ]\n[ \"$3\" = --connect-timeout ]\n[ \"$4\" = 10 ]\n[ \"$5\" = --max-time ]\n[ \"$6\" = 30 ]\n[ \"$7\" = --config ]\nscript_dir=$(CDPATH= cd -- \"$(dirname -- \"$0\")\" && pwd)\ncmp -s \"$8\" \"$script_dir/expected-curl-config\"\n",
        )
        .expect("write fake curl verifier");
        let mut permissions = fs::metadata(&binary)
            .expect("inspect fake curl")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&binary, permissions).expect("make fake curl executable");
        binary
    }

    #[cfg(unix)]
    #[test]
    fn claim_authenticated_verification_refuses_wrong_ca_hostname_or_token() {
        let directory = TempDir::new().expect("private test directory");
        let trusted_ca = directory.path().join("trusted-root.crt");
        let wrong_ca = directory.path().join("wrong-root.crt");
        fs::write(&trusted_ca, "trusted CA").expect("write trusted CA");
        fs::write(&wrong_ca, "wrong CA").expect("write wrong CA");
        let url = "https://atlas.example-tailnet.ts.net:8443/api/v1/backups";
        let verifier = write_fake_claim_verifier(
            directory.path(),
            url,
            &trusted_ca,
            "expected-local-api-token",
        );

        curl_verify_ca_hostname_and_token_with_curl(
            &verifier,
            url,
            &trusted_ca,
            b"expected-local-api-token",
            directory.path(),
        )
        .expect("the exact CA, hostname, and token must be passed to curl");
        assert!(
            curl_verify_ca_hostname_and_token_with_curl(
                &verifier,
                url,
                &wrong_ca,
                b"expected-local-api-token",
                directory.path(),
            )
            .is_err()
        );
        assert!(
            curl_verify_ca_hostname_and_token_with_curl(
                &verifier,
                "https://wrong-host.example:8443/api/v1/backups",
                &trusted_ca,
                b"expected-local-api-token",
                directory.path(),
            )
            .is_err()
        );
        assert!(
            curl_verify_ca_hostname_and_token_with_curl(
                &verifier,
                url,
                &trusted_ca,
                b"wrong-local-api-token",
                directory.path(),
            )
            .is_err()
        );
    }
}
