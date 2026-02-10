# 🔒 WSP

```
 ██╗    ██╗███████╗██████╗
 ██║    ██║██╔════╝██╔══██╗
 ██║ █╗ ██║███████╗██████╔╝
 ██║███╗██║╚════██║██╔═══╝
 ╚███╔███╔╝███████║██║
  ╚══╝╚══╝ ╚══════╝╚═╝
```

**Zero-knowledge E2EE terminal chat — ephemeral, encrypted, no metadata**

A terminal-based encrypted messenger where **everything is E2EE**, messages are **ephemeral by default** (RAM-only), and the relay server is **completely blind** to your conversations.

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg)](https://www.rust-lang.org/)

---

## 🌟 Features

- **🔐 End-to-End Encryption**: X25519 key exchange + ChaCha20-Poly1305 authenticated encryption
- **👻 Ephemeral by Default**: Messages live in RAM only — gone when you close the app
- **🕵️ Zero-Knowledge Relay**: Server stores **nothing** — no logs, no metadata, no disk writes
- **🚫 No Accounts**: Your identity is your public key. No registration, no phone numbers
- **🖥️ Beautiful TUI**: Clean terminal interface with ratatui
- **💬 Direct Messages**: Private E2EE DMs via tabbed interface — relay can't tell who's talking to who
- **👥 Group Chats**: Multi-party E2EE groups with pairwise fan-out — relay routes by room ID but stays completely blind
- **📁 Encrypted File Transfer**: Send files of any size, chunked and encrypted end-to-end (works in DMs and groups)
- **🏷️ Nicknames**: Set display names without revealing identity
- **🔄 Auto-Reconnect**: Seamless reconnection with keepalive — survives network hiccups
- **🔒 Optional Encrypted Storage**: Save chat history encrypted locally (your key only)
- **🔊 E2EE Voice Calls**: Real-time encrypted voice calls in DMs and group chats — Opus codec, ChaCha20-Poly1305 per frame, RNNoise noise suppression
- **⚡ Fast & Lightweight**: Rust-powered async networking with tokio

---

## 🎯 Philosophy

Modern chat apps harvest metadata, require phone numbers, and operate opaque servers. **WSP** is the opposite:

- **Privacy by default**: The relay server can't read your messages or metadata
- **No trust required**: You don't trust us with your identity or messages
- **Ephemeral first**: Messages disappear by default (like a real conversation)
- **Open source**: Audit the code, run your own relay

---

## 🏗️ Architecture

```
┌─────────────┐                  ┌─────────────┐
│   Alice     │                  │     Bob     │
│  (Client)   │                  │  (Client)   │
└──────┬──────┘                  └──────┬──────┘
       │                                │
       │    1. Key Exchange (E2EE)     │
       │◄──────────────────────────────►│
       │                                │
       │    2. Encrypted Messages       │
       │◄──────────────────────────────►│
       │          (blind relay)         │
       │                                │
       └────────────┬───────────────────┘
                    │
                    ▼
            ┌───────────────┐
            │  Relay Server │
            │   (blind)     │
            │               │
            │ • No storage  │
            │ • No logging  │
            │ • RAM only    │
            │ • Forwards    │
            │   encrypted   │
            │   blobs       │
            └───────────────┘
```

### How It Works

1. **Identity Generation**: Each user generates an X25519 keypair (stored locally, encrypted with password)
2. **Connect to Relay**: Client connects to WebSocket relay, gets ephemeral session ID
3. **Key Exchange**: Clients perform X25519 Diffie-Hellman key exchange
4. **Encrypted Chat**: All messages encrypted with ChaCha20-Poly1305, relayed as opaque blobs
5. **Zero Metadata**: Server doesn't know who talks to who (session IDs are random)

---

## 🚀 Installation

### Build from Source

```bash
git clone https://github.com/Bentlybro/wsp.git
cd wsp
cargo build --release
./target/release/wsp --help
```

#### Windows Build Note

Voice calls require Opus (built via CMake). If you get a CMake policy error:

```cmd
set CMAKE_POLICY_VERSION_MINIMUM=3.5
cargo build --release
```

### Install with Cargo

```bash
cargo install wsp
```

---

## 📖 Usage

### 1. Generate Your Identity

```bash
wsp init
```

This creates an encrypted keypair at `~/.wsp/identity`. **Keep this safe!**

You'll get a public ID like:
```
YourPublicKey: abc123def456...
```

### 2. Run a Relay Server (Optional)

To host your own relay:

```bash
wsp relay --addr 0.0.0.0:8080
```

**The relay is zero-knowledge:**
- No disk writes
- No logging
- RAM-only
- Blind message forwarding

### 3. Start Chatting

Connect to a relay and chat:

```bash
wsp chat --relay ws://localhost:8080
```

### 4. TUI Commands

| Command | Description |
|---------|-------------|
| `/nick <name>` | Set your display nickname |
| `/dm <nickname\|peer_id>` | Open a direct message tab |
| `/group create <name>` | Create a new encrypted group chat |
| `/group invite <peer>` | Invite a peer to the current group |
| `/group leave` | Leave the current group |
| `/group members` | List members of the current group |
| `/call` | Start an E2EE voice call (DM or Group tab) |
| `/accept-call` | Accept an incoming voice call (DM or group) |
| `/reject-call` | Reject an incoming voice call (DM or group) |
| `/hangup` | End/leave the current voice call |
| `/mute` | Toggle microphone mute during a call |
| `/send <filepath>` | Send an encrypted file to the current tab |
| `/accept <save_path>` | Accept an incoming file transfer |
| `/reject` | Reject an incoming file transfer |
| `Tab` / `Shift+Tab` | Switch between chat tabs |
| `Shift+Enter` | Insert newline |
| `Enter` | Send message |
| `Ctrl+C` | Quit |

### 5. Optional: Save Chat History

By default, messages are ephemeral (RAM-only). To save encrypted history:

```bash
wsp chat --relay ws://localhost:8080 --save
```

History is encrypted with your identity key and stored locally.

---

## 🔐 Security Model

### What WSP Protects

✅ **Message Content**: Encrypted with ChaCha20-Poly1305

✅ **Metadata**: Session IDs are random, rotated

✅ **Forward Secrecy**: Planned with Double Ratchet protocol

✅ **Zero Server Storage**: Relay stores nothing to disk

### What WSP Does NOT Protect

❌ **Network Metadata**: Your ISP can see you connect to the relay

❌ **Endpoint Security**: If your device is compromised, messages can be read

❌ **Relay Availability**: If relay goes down, you're disconnected

❌ **Traffic Analysis**: Relay sees connection timing (but not content)

### Recommended Usage

- **Use Tor/VPN** if network anonymity is critical
- **Run your own relay** for maximum trust
- **Verify keys out-of-band** (e.g., in person, via Signal)

---

## 🆚 Comparison to Alternatives

| Feature                  | WSP     | Signal | Matrix | IRC   |
|--------------------------|---------|--------|--------|-------|
| E2EE                     | ✅      | ✅     | ✅*    | ❌    |
| No Phone Number          | ✅      | ❌     | ✅     | ✅    |
| Zero-Knowledge Server    | ✅      | ❌**   | ❌     | ❌    |
| Ephemeral by Default     | ✅      | ❌     | ❌     | ❌    |
| Terminal-Based           | ✅      | ❌     | ✅***  | ✅    |
| Open Source              | ✅      | ✅     | ✅     | ✅    |
| Self-Hostable Relay      | ✅      | ❌     | ✅     | ✅    |

\* Matrix E2EE requires setup
\** Signal server knows metadata
\*** With third-party clients like weechat-matrix

**WSP is for when you want:**
- Maximum privacy (zero-knowledge relay)
- No accounts/registration
- Ephemeral conversations by default
- Terminal-only workflow

---

## 🗺️ Roadmap

### MVP (v0.1) ✅
- [x] X25519 key exchange
- [x] ChaCha20-Poly1305 encryption
- [x] Blind WebSocket relay
- [x] TUI with ratatui
- [x] Ephemeral messages (RAM-only)
- [x] Optional encrypted local storage

### v0.2 ✅
- [x] **Direct Messages** (private E2EE tabs, client-side routing)
- [x] **Nicknames** (`/nick` command, broadcast to peers)
- [x] **Encrypted File Transfer** (`/send`, `/accept`, `/reject` — chunked, any size)
- [x] **Auto-Reconnect** (keepalive pings, automatic reconnection with backoff)

### v0.3 ✅
- [x] **Group Chats** (multi-party E2EE with pairwise fan-out — relay stays blind)
  - `/group create <name>` — create a new encrypted group
  - `/group invite <peer>` — invite peers via encrypted DM
  - `/group leave` — leave the current group
  - `/group members` — list group members
  - File transfer works in groups too
- [x] **Forward-Compatible Serialization** (MessagePack replaces bincode — new fields won't break older clients)

### v0.4 ✅
- [x] **E2EE Voice Calls** — Real-time encrypted voice calls in DMs and groups
  - `/call` — initiate a voice call in a DM or Group tab
  - `/accept-call` / `/reject-call` — respond to incoming calls
  - `/hangup` — end/leave the current call
  - Group calls: audio fan-out to all group members with pairwise encryption
  - Opus codec (48kHz mono, 20ms frames) → ChaCha20-Poly1305 encryption → WebSocket transport
  - **RNNoise noise suppression** — removes background noise (keyboard, fans, AC, breathing) in real-time
  - Lock-free ring buffer playback for glitch-free audio on Linux/ALSA
  - Status bar shows active call with duration timer

### Planned Features
- [ ] **Double Ratchet Protocol** (forward secrecy like Signal)
- [ ] **Peer-to-Peer Mode** (no relay required)
- [ ] **QR Code Identity Sharing** (for mobile)
- [ ] **Relay Discovery** (DHT or central directory)
- [ ] **Voice Messages** (encrypted audio clips)

---

## 🛠️ Development

### Tech Stack

- **Rust** (latest stable)
- **tokio** (async runtime)
- **tokio-tungstenite** (WebSocket)
- **ratatui** (TUI framework)
- **x25519-dalek** (elliptic curve cryptography)
- **chacha20poly1305** (authenticated encryption)
- **blake3** (key derivation)
- **cpal** (cross-platform audio I/O)
- **audiopus** (Opus codec for voice)
- **nnnoiseless** (RNNoise noise suppression, pure Rust)

### Build & Test

```bash
# Build
cargo build

# Run tests
cargo test

# Lint
cargo clippy

# Format
cargo fmt
```

### Contributing

PRs welcome! Please:
1. Run `cargo fmt` and `cargo clippy` before submitting
2. Add tests for new features
3. Update README if adding user-facing changes

---

## 📜 License

MIT License — see [LICENSE](LICENSE) for details.

---

## ⚠️ Disclaimer

**WSP is experimental software.** While we use industry-standard cryptography, this has not been audited. Use at your own risk.

For high-stakes communications, use audited tools like Signal or GPG.

---

## 🙏 Acknowledgments

- **Signal** for pioneering E2EE messaging
- **Matrix** for decentralized chat architecture
- **Dalek Cryptography** for Rust crypto libraries
- **ratatui** for the awesome TUI framework

---

**Made with 🔒 and ❤️ by [Bentlybro](https://github.com/Bentlybro)**

*"Privacy is not a crime"*
