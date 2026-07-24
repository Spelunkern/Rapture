use anyhow::Result;
use rapture_common::crypto::{AesCtr, expanded_xor_transform};
use rapture_common::hex::to_hex;
use rapture_common::packet::read_le_u32;
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::world_entities::lock_or_recover;

/// Upper bound on simultaneously connected clients. Protects against basic
/// connection-flood exhaustion (one OS thread is spawned per connection).
pub(crate) const MAX_CONCURRENT_CONNECTIONS: usize = 500;
/// A player can sit idle (login/select screens, or in-game doing nothing)
/// indefinitely; that is not the same as being disconnected. Liveness is
/// detected via TCP keepalive (see apply_keepalive), not an application
/// read timeout -- an idle-but-connected socket must never be reaped.
pub(crate) const KEEPALIVE_IDLE: Duration = Duration::from_secs(10);
pub(crate) const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(5);

/// Enables TCP keepalive so a connection whose peer actually vanished
/// (crash, network drop) is detected by the OS and this thread's blocking
/// read() eventually errors out -- without ever penalizing a client that
/// is simply idle but still genuinely connected.
pub(crate) fn apply_keepalive(stream: &TcpStream, logger: &Logger) {
    let keepalive = socket2::TcpKeepalive::new()
        .with_time(KEEPALIVE_IDLE)
        .with_interval(KEEPALIVE_INTERVAL);
    if let Err(error) = socket2::SockRef::from(stream).set_tcp_keepalive(&keepalive) {
        logger.line(format!(
            "Rapture World failed to enable TCP keepalive: {error:?}"
        ));
    }
}

/// Disables Nagle's algorithm on a client socket. Without this, the OS can
/// hold small outgoing packets (a skill ack, an SP/MP update, a mob damage
/// broadcast -- most of what this protocol sends) in the kernel send
/// buffer for tens to a couple hundred milliseconds waiting to coalesce
/// them with more data or a delayed ACK, even though the server resolved
/// the underlying game logic instantly. Confirmed as the likely cause of a
/// reported "the spell works immediately but the visual effect appears
/// late" bug: nothing in this project's own skill-resolution logic (nor in
/// Imgeneus's, for instant-cast skills) introduces any deliberate delay,
/// so the lag was happening below the application layer, in the socket
/// itself.
pub(crate) fn apply_nodelay(stream: &TcpStream, logger: &Logger) {
    if let Err(error) = stream.set_nodelay(true) {
        logger.line(format!("Rapture World failed to disable Nagle's algorithm: {error:?}"));
    }
}

pub(crate) static ACTIVE_CONNECTIONS: AtomicUsize = AtomicUsize::new(0);

pub(crate) struct ConnectionGuard;

impl ConnectionGuard {
    pub(crate) fn acquire(logger: &Logger) -> Option<Self> {
        loop {
            let current = ACTIVE_CONNECTIONS.load(Ordering::Relaxed);
            if current >= MAX_CONCURRENT_CONNECTIONS {
                logger.line(format!(
                    "Rapture World rejected connection: at capacity ({current}/{MAX_CONCURRENT_CONNECTIONS})"
                ));
                return None;
            }
            if ACTIVE_CONNECTIONS
                .compare_exchange(current, current + 1, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
            {
                return Some(ConnectionGuard);
            }
        }
    }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        ACTIVE_CONNECTIONS.fetch_sub(1, Ordering::SeqCst);
    }
}

fn active_accounts() -> &'static Mutex<HashSet<u32>> {
    static ACTIVE_ACCOUNTS: OnceLock<Mutex<HashSet<u32>>> = OnceLock::new();
    ACTIVE_ACCOUNTS.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Prevents the same account from having two live World sessions at once.
/// Held for the lifetime of a connection's handle_client call; released
/// automatically (Drop) on disconnect, error, or logout, regardless of
/// which exit path is taken.
pub(crate) struct AccountSessionGuard {
    account_id: u32,
}

impl AccountSessionGuard {
    pub(crate) fn try_acquire(account_id: u32, logger: &Logger) -> Option<Self> {
        let mut accounts = lock_or_recover(active_accounts());
        if !accounts.insert(account_id) {
            logger.line(format!(
                "Rapture World rejected login for account_id={account_id}: already connected from another session"
            ));
            return None;
        }
        Some(AccountSessionGuard { account_id })
    }
}

impl Drop for AccountSessionGuard {
    fn drop(&mut self) {
        lock_or_recover(active_accounts()).remove(&self.account_id);
    }
}

pub(crate) struct Logger {
    pub(crate) file: Mutex<std::fs::File>,
}

// Every client connection runs on its own OS thread, so a thread-local tag
// is the simplest way to distinguish interleaved log lines from concurrent
// connections without changing every function that takes `logger: &Logger`.
// Without this, lines from two connections appear merged in file order,
// which looks exactly like packet/decryption corruption when it is not.
thread_local! {
    static CONNECTION_TAG: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

pub(crate) fn set_connection_tag(tag: String) {
    CONNECTION_TAG.with(|cell| *cell.borrow_mut() = tag);
}

impl Logger {
    pub(crate) fn new(path: &str) -> Result<Self> {
        fs::create_dir_all("logs")?;
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            file: Mutex::new(file),
        })
    }

    pub(crate) fn line(&self, message: impl AsRef<str>) {
        let message = message.as_ref();
        let tagged = CONNECTION_TAG.with(|cell| {
            let tag = cell.borrow();
            if tag.is_empty() {
                message.to_string()
            } else {
                format!("[{tag}] {message}")
            }
        });
        println!("{tagged}");
        if let Ok(mut file) = self.file.lock() {
            let _ = writeln!(file, "{tagged}");
            let _ = file.flush();
        }
    }
}

#[derive(Clone)]
pub(crate) struct WorldSession {
    pub(crate) identity: [u8; 16],
    pub(crate) key: [u8; 16],
    pub(crate) login_iv: [u8; 16],
    pub(crate) account_id: u32,
}

/// Session handoff is keyed per-account instead of a single shared file, so
/// concurrent logins from different accounts no longer race on one path.
/// The real client's first World-bound packet (GameHandshake, 0xA301)
/// already carries the account id, so World looks up the exact session that
/// Login prepared for this specific connection.
pub(crate) const WORLD_SESSION_MAX_AGE: Duration = Duration::from_secs(30);

pub(crate) fn world_session_path(account_id: u32) -> String {
    format!("runtime/sessions/{account_id}.bin")
}

pub(crate) fn load_world_session(account_id: u32) -> Result<Option<WorldSession>> {
    let path = world_session_path(account_id);
    let Ok(metadata) = std::fs::metadata(&path) else {
        return Ok(None);
    };

    if let Ok(modified) = metadata.modified()
        && modified.elapsed().unwrap_or_default() > WORLD_SESSION_MAX_AGE
    {
        let _ = std::fs::remove_file(&path);
        return Ok(None);
    }

    let mut file = std::fs::File::open(&path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    drop(file);
    // Single-use: consume the file immediately so a duplicate/replayed
    // World connection for the same account cannot reuse this session.
    let _ = std::fs::remove_file(&path);

    if bytes.len() < 48 {
        anyhow::bail!("world session file is too short");
    }

    let stored_account_id = if bytes.len() >= 52 {
        read_le_u32(&bytes[48..52])?
    } else {
        account_id
    };

    if stored_account_id != account_id {
        anyhow::bail!(
            "world session account mismatch: file={stored_account_id} requested={account_id}"
        );
    }

    Ok(Some(WorldSession {
        identity: bytes[0..16].try_into()?,
        key: bytes[16..32].try_into()?,
        login_iv: bytes[32..48].try_into()?,
        account_id: stored_account_id,
    }))
}

pub(crate) fn encrypt_packet(aes: &mut AesCtr, use_expanded_key: bool, packet: &[u8]) -> Vec<u8> {
    if packet.len() <= 2 {
        return packet.to_vec();
    }

    let mut encrypted = packet[..2].to_vec();
    if use_expanded_key {
        encrypted.extend_from_slice(&expanded_xor_transform(&packet[2..]));
    } else {
        encrypted.extend_from_slice(&aes.transform(&packet[2..]));
    }
    encrypted
}

pub(crate) fn send_encrypted(
    stream: &mut TcpStream,
    aes: &mut AesCtr,
    use_expanded_key: bool,
    packet: Vec<u8>,
    label: &str,
    logger: &Logger,
) -> Result<()> {
    let encrypted = encrypt_packet(aes, use_expanded_key, &packet);
    logger.line(format!(
        "Rapture World sending {label} raw (expanded_key={use_expanded_key}): {}",
        to_hex(&packet)
    ));
    logger.line(format!(
        "Rapture World sending {label} encrypted: {}",
        to_hex(&encrypted)
    ));
    stream.write_all(&encrypted)?;
    Ok(())
}
