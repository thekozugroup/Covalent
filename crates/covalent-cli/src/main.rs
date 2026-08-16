use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use covalent_core::{
    AuthorizedRoot, BackupOptions, Engine, EngineOptions, JobControl, PairingSession,
    RestoreOptions, RestorePlan,
};
use covalent_protocol::{
    BackupId, ConflictPolicy, DeviceId, ExportedDeviceSettings, PROTOCOL_VERSION,
    PairingInvitation, PeerRole, RelativePath, ReplicaAvailability, ReplicaIntent,
};
use serde::Serialize;

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
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
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
        /// Stable resumable job identifier.
        #[arg(long, default_value = "cli-backup")]
        job_id: String,
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
    let arguments = Arguments::parse();
    match arguments.command {
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
            let engine = open_engine(arguments.data_dir)?;
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
            let engine = open_engine(arguments.data_dir)?;
            print_json(&engine.list_backups()?)
        }
        Command::Daemon {
            listen,
            peer_listen,
        } => {
            let status = ProcessCommand::new("covalent-node")
                .args([
                    "serve",
                    "--listen",
                    &listen,
                    "--peer-listen",
                    &peer_listen,
                    "--data-dir",
                ])
                .arg(arguments.data_dir)
                .status()
                .context("start covalent-node daemon")?;
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
            let engine = open_engine(arguments.data_dir)?;
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
            let engine = open_engine(arguments.data_dir)?;
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
            let engine = open_engine(arguments.data_dir)?;
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
            let engine = open_engine(arguments.data_dir)?;
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
            let engine = open_engine(arguments.data_dir)?;
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
            let engine = open_engine(arguments.data_dir)?;
            let backup_id = backup_id
                .as_deref()
                .map(parse_backup_id)
                .transpose()?
                .unwrap_or_default();
            let selected: Result<Vec<_>> = providers
                .iter()
                .map(|provider| parse_device_id(provider))
                .collect();
            let mut options = BackupOptions::new(backup_id, snapshot_id, job_id);
            options.display_name = name;
            options.created_at_unix_ms = now_unix_ms();
            options.replica_intent = ReplicaIntent::explicit(selected?);
            let result = engine.backup(source, &options, &JobControl::new(), |_| {})?;
            print_json(&BackupReport {
                backup_id: backup_id.to_string(),
                snapshot_id: result.manifest.snapshot_id,
                entries: result.manifest.entries.len(),
                bytes_read: result.progress.bytes_read,
                chunks_stored: result.progress.chunks_stored,
                chunks_deduplicated: result.progress.chunks_deduplicated,
                selected_providers: result.manifest.replica_intent.selected_providers.len(),
                provider_failures: result.replication.failures.len(),
            })
        }
        Command::Verify {
            backup_id,
            snapshot_id,
            repair,
            providers,
        } => {
            let engine = open_engine(arguments.data_dir)?;
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
            let engine = open_engine(arguments.data_dir)?;
            let plan = engine.preview_restore(
                parse_backup_id(&backup_id)?,
                &snapshot_id,
                target,
                &RestoreOptions {
                    conflict_policy: conflict.into(),
                    selected_paths: Default::default(),
                    job_id,
                },
            )?;
            let bytes = serde_json::to_vec_pretty(&plan)?;
            write_or_print(output.as_deref(), &bytes)
        }
        Command::RestoreExecute { plan } => {
            let engine = open_engine(arguments.data_dir)?;
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
            let engine = open_engine(arguments.data_dir)?;
            write_or_print(output.as_deref(), &engine.export_settings()?)
        }
        Command::ConfigImport { input, confirm } => {
            let engine = open_engine(arguments.data_dir)?;
            let bytes = read_bounded(&input, 1_048_576)?;
            engine.import_settings(&bytes, confirm)?;
            println!("settings imported");
            Ok(())
        }
    }
}

fn open_engine(data_directory: PathBuf) -> Result<Engine> {
    Engine::open(EngineOptions::new(data_directory)).context("open Covalent engine")
}

fn parse_backup_id(value: &str) -> Result<BackupId> {
    BackupId::from_str(value).with_context(|| format!("invalid backup ID {value}"))
}

fn parse_device_id(value: &str) -> Result<DeviceId> {
    DeviceId::from_str(value).with_context(|| format!("invalid device ID {value}"))
}

fn read_bounded(path: &Path, maximum: u64) -> Result<Vec<u8>> {
    let metadata = fs::metadata(path).with_context(|| format!("inspect {}", path.display()))?;
    if metadata.len() > maximum {
        bail!("{} exceeds the {} byte limit", path.display(), maximum);
    }
    fs::read(path).with_context(|| format!("read {}", path.display()))
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
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}
