use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "wsp")]
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
        #[arg(short, long, default_value = "~/.wsp/identity")]
        path: String,
    },
    
    /// Start a chat session
    Chat {
        /// Relay server URL
        #[arg(short, long, default_value = "ws://localhost:8899")]
        relay: String,

        /// Identity file path
        #[arg(short, long, default_value = "~/.wsp/identity")]
        identity: String,

        /// Save chat history (encrypted locally)
        #[arg(short, long)]
        save: bool,

        /// Your nickname (visible to other users after E2EE)
        #[arg(short, long)]
        name: Option<String>,
    },
    
    /// Run a relay server
    Relay {
        /// Address to bind to
        #[arg(short, long, default_value = "0.0.0.0:8899")]
        addr: String,
    },
}

impl Cli {
    pub fn parse_args() -> Self {
        Self::parse()
    }
}
