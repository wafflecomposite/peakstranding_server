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
The server reads configuration from environment variables (see the provided `.env` for defaults). At minimum, supply a Steam dev API key:

```
 STEAM_WEB_API_KEY=YOUR_KEY_HERE
```

The following knobs are optional and fall back to sensible defaults:

- `STEAM_APPID` (default 3527290) – Steam AppID used when validating auth tickets.
- `MAX_USER_STRUCTS_SAVED_PER_SCENE` (default 100) – Maximum stored structures per user/scene before pruning the oldest.
- `MAX_REQUESTED_STRUCTS` (default 300) – Upper bound for a single random structures fetch.
- `POST_STRUCTURE_RATE_LIMIT` (default 2) – Seconds between structure submissions per user.
- `GET_STRUCTURE_RATE_LIMIT` (default 6) – Seconds between random-structure reads per user.
- `POST_LIKE_RATE_LIMIT` (default 1) – Seconds between like requests per user.
- `DEFAULT_RANDOM_LIMIT` (default 30) – Default number of structures returned when a client omits `limit`.
- `MAX_SCENE_LENGTH` (default 50) – Maximum allowed characters for scene identifiers.
- `DATABASE_URL` (default `sqlite://peakstranding.db?mode=rwc`) – SQLx connection string.
- `SERVER_PORT` (default 3000) – TCP port the listener binds to.

## Running
```bash
./target/release/peakstranding_server
```
The server listens on TCP port 3000 by default (override with `SERVER_PORT`).  

## What’s next?
- Basic metrics and health-check endpoint
- Containerized release workflow

Contributions are welcome.  

