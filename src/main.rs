mod audio;
mod cli;
mod client;
mod crypto;
mod protocol;
mod relay;
mod storage;
mod tui;

use anyhow::{Context, Result};
use cli::{Cli, Commands};
use crypto::Identity;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse_args();

    match cli.command {
        Commands::Init { path } => {
            let path = expand_path(&path);
            init_identity(&path).await?;
        }
        Commands::Chat {
            relay,
            identity,
            save,
            name,
        } => {
            let identity_path = expand_path(&identity);
            start_chat(&relay, &identity_path, save, name).await?;
        }
        Commands::Relay { addr } => {
            relay::start_relay(addr).await?;
        }
    }

    Ok(())
}

async fn init_identity(path: &PathBuf) -> Result<()> {
    println!("🔐 Generating new identity...");

    // Create directory if needed
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Check if identity already exists
    if path.exists() {
        println!("⚠️  Identity already exists at: {}", path.display());
        println!("Do you want to overwrite? (y/N): ");
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Aborted.");
            return Ok(());
        }
    }

    let identity = Identity::generate();
    let public_key = identity.public_key_b64();

    println!("Enter a password to encrypt your identity:");
    let password = rpassword::read_password()?;

    println!("Confirm password:");
    let password_confirm = rpassword::read_password()?;

    if password != password_confirm {
        anyhow::bail!("Passwords do not match");
    }

    identity.save_to_file(path, &password)?;

    println!("✅ Identity saved to: {}", path.display());
    println!();
    println!("📋 Your public ID (share this to receive messages):");
    println!("{}", public_key);
    println!();
    println!("⚠️  Keep your identity file and password safe!");

    Ok(())
}

async fn start_chat(relay_url: &str, identity_path: &PathBuf, _save_history: bool, nickname: Option<String>) -> Result<()> {
    // Load identity
    println!("🔐 Loading identity from: {}", identity_path.display());
    println!("Enter password:");
    let password = rpassword::read_password()?;

    let identity = Identity::load_from_file(identity_path, &password)
        .context("Failed to load identity (wrong password?)")?;

    println!("✅ Identity loaded");
    println!("📋 Your ID: {}", identity.public_key_b64());
    if let Some(ref nick) = nickname {
        println!("👤 Nickname: {}", nick);
    }
    println!();
    println!("🔌 Connecting to relay: {}", relay_url);

    let mut client = client::ChatClient::new(identity, relay_url.to_string(), nickname.clone());
    let _own_id = client.identity_id();
    let session_id = client.session_id().to_string();

    println!("🔗 Session ID: {}", session_id);
    println!();

    let (msg_tx, incoming_rx, status_rx, peer_update_rx, audio_in_rx) = client.connect().await?;

    println!("✅ Connected! Share your Session ID with peers to start chatting.");
    println!("Starting TUI...");
    println!();

    // Small delay to let connection establish
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let mut ui = tui::ChatUI::new(session_id, nickname);
    ui.run(msg_tx, incoming_rx, status_rx, peer_update_rx, audio_in_rx).await?;

    Ok(())
}

fn expand_path(path: &str) -> PathBuf {
    if let Some(stripped) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            let mut buf = PathBuf::from(home);
            buf.push(stripped);
            return buf;
        }
    }
    PathBuf::from(path)
}
