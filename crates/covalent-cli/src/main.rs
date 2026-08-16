use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use covalent_core::AuthorizedRoot;
use covalent_protocol::{ExportedDeviceSettings, PROTOCOL_VERSION, RelativePath};
use serde::Serialize;

#[derive(Debug, Parser)]
#[command(name = "covalent", version, about = "Covalent backup operator CLI")]
struct Arguments {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Prints deterministic foundation diagnostics.
    Doctor,
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
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DoctorReport {
    status: &'static str,
    protocol_version: u16,
    memory_safe_engine: bool,
    external_account_required: bool,
    automatic_replica_placement: bool,
    restore_requires_authorized_root: bool,
    tier1: [&'static str; 4],
    tier2: [&'static str; 1],
}

fn main() -> Result<()> {
    match Arguments::parse().command {
        Command::Doctor => print_json(&DoctorReport {
            status: "ok",
            protocol_version: PROTOCOL_VERSION,
            memory_safe_engine: true,
            external_account_required: false,
            automatic_replica_placement: false,
            restore_requires_authorized_root: true,
            tier1: ["macOS", "Android", "Docker", "Unraid"],
            tier2: ["iOS"],
        }),
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
    }
}

fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
