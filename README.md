# Peak Stranding Server

Prototype multiplayer backend for *[Peak Stranding](https://thunderstore.io/c/peak/p/lnkr/PeakStranding/)* - a PEAK game mod.  
Tested on Ubuntu 22.04 LTS.

---

## Prerequisites
```bash
sudo apt update && sudo apt upgrade -y
sudo apt install -y git curl build-essential pkg-config libssl-dev
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
# Activate Rust for this shell
source "$HOME/.cargo/env"
```

## Clone & Build
```bash
git clone https://github.com/wafflecomposite/peakstranding_server.git
cd peakstranding_server
cargo build --release        # use --verbose for more output
```
The optimized binary is at target/release/peakstranding_server.  
For iterative development builds, run `cargo build`.  

## Configuration
The server needs a [Steam dev API key](https://steamcommunity.com/dev/apikey) to resolve profile data.  
Create a .env file in the project root (or export the variable in your shell):  
`STEAM_WEB_API_KEY=YOUR_KEY_HERE`

## Running
```bash
./target/release/peakstranding_server
```
The server listens on TCP port 3000 by hard-coded default.  

## What’s next?
- Basic metrics and health-check endpoint
- Containerized release workflow

Contributions are welcome.  