use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "whisper")]
#[command(about = "🔒 Zero-knowledge E2EE terminal chat", long_about = None)]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Initialize a new identity (generates keypair)
    Init {
        /// Path to save identity file
        #[arg(short, long, default_value = "~/.whisper/identity")]
        path: String,
    },
    
    /// Start a chat session
    Chat {
        /// Relay server URL
        #[arg(short, long, default_value = "ws://localhost:8080")]
        relay: String,

        /// Identity file path
        #[arg(short, long, default_value = "~/.whisper/identity")]
        identity: String,

        /// Save chat history (encrypted locally)
        #[arg(short, long)]
        save: bool,
    },
    
    /// Run a relay server
    Relay {
        /// Address to bind to
        #[arg(short, long, default_value = "127.0.0.1:8080")]
        addr: String,
    },
}

impl Cli {
    pub fn parse_args() -> Self {
        Self::parse()
    }
}
