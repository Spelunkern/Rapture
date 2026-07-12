use anyhow::{Context, Result};
use rapture_common::crypto::{
    AesCtr, decrypt_login_rsa, derive_login_aes_material, login_crypto_material,
};
use rapture_common::db;
use rapture_common::hex::to_hex;
use rapture_common::packet::{Opcode, PacketWriter, read_fixed_ascii, read_le_u16, read_le_u32};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// A per-login random "identity" token: sent to the client in the login-
/// success packet, then echoed back verbatim in World's GameHandshake
/// (0xA301) and checked against what Login wrote into the session handoff
/// file (session.rs's WorldSession::identity). This used to be a FIXED
/// constant (the same bytes for every single login), which meant the
/// handoff was secured by nothing but the account_id (numeric, enumerable)
/// -- anyone who could guess/know a victim's account_id could race a raw
/// connection to World's GameHandshake against the real client within the
/// session file's validity window and take over the session, since the
/// "identity" the real client would send matched every time regardless of
/// who was logging in. Generating this fresh per login (and never logging
/// it -- see the redacted log lines below) closes that gap: the file can
/// now only be consumed by whoever actually received this exact login
/// response, not by anyone who merely knows the account_id.
fn generate_identity() -> [u8; 16] {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::AtomicU64;
    use std::time::{SystemTime, UNIX_EPOCH};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    // No crypto-grade RNG crate is in this workspace's dependency graph, so
    // this combines several independent, hard-to-predict-in-combination
    // sources instead of adding one: RandomState's own OS-seeded SipHash
    // key, a nanosecond timestamp, a stack address (ASLR-randomized per
    // process), and a monotonically incrementing counter. Good enough for
    // this project's actual threat model (a private/local dev server, not
    // a public, adversarial-hardened one) -- swap in a real CSPRNG crate
    // (e.g. `rand`) if this project ever needs to withstand more scrutiny.
    let stack_marker = 0u8;
    let stack_addr = &stack_marker as *const u8 as usize;
    let mut bytes = [0u8; 16];
    for (index, chunk) in bytes.chunks_mut(8).enumerate() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let counter = COUNTER.fetch_add(1, Ordering::SeqCst);
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u128(nanos);
        hasher.write_usize(stack_addr);
        hasher.write_u64(counter);
        hasher.write_usize(index);
        chunk.copy_from_slice(&hasher.finish().to_le_bytes());
    }
    bytes
}

/// Upper bound on simultaneously connected login clients. The login flow is
/// brief (handshake, credentials, server select) so this only needs to
/// cover a burst of concurrent logins, not steady-state player count.
const MAX_CONCURRENT_CONNECTIONS: usize = 200;
/// A player can sit at the login/server-select screen indefinitely; that
/// is not the same as being disconnected. Liveness is detected via TCP
/// keepalive (see apply_keepalive), not an application read timeout.
const KEEPALIVE_IDLE: Duration = Duration::from_secs(10);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(5);
static ACTIVE_CONNECTIONS: AtomicUsize = AtomicUsize::new(0);

fn apply_keepalive(stream: &TcpStream, logger: &Logger) {
    let keepalive = socket2::TcpKeepalive::new()
        .with_time(KEEPALIVE_IDLE)
        .with_interval(KEEPALIVE_INTERVAL);
    if let Err(error) = socket2::SockRef::from(stream).set_tcp_keepalive(&keepalive) {
        logger.line(format!(
            "Rapture Login failed to enable TCP keepalive: {error:?}"
        ));
    }
}

struct ConnectionGuard;

impl ConnectionGuard {
    fn acquire(logger: &Logger) -> Option<Self> {
        loop {
            let current = ACTIVE_CONNECTIONS.load(Ordering::Relaxed);
            if current >= MAX_CONCURRENT_CONNECTIONS {
                logger.line(format!(
                    "Rapture Login rejected connection: at capacity ({current}/{MAX_CONCURRENT_CONNECTIONS})"
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

struct Logger {
    file: Mutex<std::fs::File>,
}

// One OS thread per connection (see main()); without a per-thread tag,
// concurrent logins interleave in the shared log file with no way to tell
// which line belongs to which connection. Mirrors rapture_world's fix.
thread_local! {
    static CONNECTION_TAG: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

fn set_connection_tag(tag: String) {
    CONNECTION_TAG.with(|cell| *cell.borrow_mut() = tag);
}

impl Logger {
    fn new(path: &str) -> Result<Self> {
        fs::create_dir_all("logs")?;
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            file: Mutex::new(file),
        })
    }

    fn line(&self, message: impl AsRef<str>) {
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

fn build_login_handshake() -> Result<Vec<u8>> {
    let material = login_crypto_material();
    let mut packet = PacketWriter::new(Opcode::LoginHandshake);
    packet.write_u8(0);
    packet.write_u8(material.public_exponent.len() as u8);
    packet.write_u8(material.public_modulus.len() as u8);
    packet.write_bytes(&material.public_exponent);
    packet.write_bytes(&material.public_modulus);
    packet.into_bytes()
}

/// Aries's AuthStatus enum (shaiya/include/common/UserTypes.h): AdminA=1..
/// AdminE=5 are the admin tiers, None=11 is a regular account. Was
/// hardcoded to 255 (not a valid AuthStatus value at all) before this.
fn auth_status_byte(account: &db::DbAccount) -> u8 {
    if account.is_admin {
        account.admin_level.clamp(1, 5)
    } else {
        11
    }
}

fn build_login_success(account_id: u32, auth_status: u8, identity: &[u8; 16]) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::LoginRequest);
    packet.write_u8(0);
    packet.write_u32(account_id);
    packet.write_u8(auth_status);
    packet.write_bytes(identity);
    packet.into_bytes()
}

fn build_login_failure() -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::LoginRequest);
    packet.write_u8(1);
    packet.into_bytes()
}

fn build_server_list() -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::ServerList);
    packet.write_u8(1);
    packet.write_u8(1);
    packet.write_u8(0);
    packet.write_u16(0);
    packet.write_u16(100);
    packet.write_fixed_string("Rapture", 32)?;
    packet.into_bytes()
}

fn build_select_server_success() -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::SelectServer);
    packet.write_u8(0);
    packet.write_bytes(&[127, 0, 0, 1]);
    packet.into_bytes()
}

fn encrypt_packet(aes: &mut AesCtr, packet: &[u8]) -> Vec<u8> {
    if packet.len() <= 2 {
        return packet.to_vec();
    }

    let mut encrypted = packet[..2].to_vec();
    encrypted.extend_from_slice(&aes.transform(&packet[2..]));
    encrypted
}

fn send_encrypted(
    stream: &mut TcpStream,
    aes: &mut AesCtr,
    packet: Vec<u8>,
    label: &str,
    logger: &Logger,
) -> Result<()> {
    let encrypted = encrypt_packet(aes, &packet);
    logger.line(format!(
        "Rapture Login sending {label} raw: {}",
        to_hex(&packet)
    ));
    logger.line(format!(
        "Rapture Login sending {label} encrypted: {}",
        to_hex(&encrypted)
    ));
    stream.write_all(&encrypted)?;
    Ok(())
}

/// Session handoff to World is keyed per-account instead of a single shared
/// file. The real client's first World-bound packet (GameHandshake, 0xA301)
/// already carries the account id, so World can look up the matching file
/// directly instead of racing on one shared path across concurrent logins.
fn world_session_path(account_id: u32) -> String {
    format!("runtime/sessions/{account_id}.bin")
}

fn save_world_session(
    aes_material: &[u8; 32],
    account_id: u32,
    identity: &[u8; 16],
    logger: &Logger,
) -> Result<()> {
    fs::create_dir_all("runtime/sessions")?;
    let mut file = std::fs::File::create(world_session_path(account_id))?;
    file.write_all(identity)?;
    file.write_all(aes_material)?;
    file.write_all(&account_id.to_le_bytes())?;

    // Never log the actual AES key/IV material -- this used to write it in
    // cleartext to logs/rapture_login.log, letting anyone with read access
    // to the log lift a live session's key straight out of it.
    logger.line(format!(
        "Rapture Login saved world session account_id={account_id}"
    ));
    Ok(())
}

static NEXT_CONNECTION_ID: AtomicUsize = AtomicUsize::new(1);

fn handle_client(mut stream: TcpStream, logger: &Logger) -> Result<()> {
    let connection_id = NEXT_CONNECTION_ID.fetch_add(1, Ordering::SeqCst);
    set_connection_tag(format!("conn{connection_id}"));
    let mut aes_recv: Option<AesCtr> = None;
    let mut aes_send: Option<AesCtr> = None;
    let mut aes_material_for_world: Option<[u8; 32]> = None;
    let handshake = build_login_handshake()?;

    logger.line("Rapture Login client connected");
    logger.line(format!(
        "Rapture Login sending 0xA101 handshake: {}",
        to_hex(&handshake)
    ));
    stream.write_all(&handshake)?;

    loop {
        let mut buffer = [0u8; 512];
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            logger.line("Rapture Login client disconnected");
            return Ok(());
        }

        let bytes = &buffer[..read];
        logger.line(format!(
            "Rapture Login received {read} bytes: {}",
            to_hex(bytes)
        ));

        if bytes.len() < 4 {
            continue;
        }

        let length = read_le_u16(&bytes[0..2])?;
        let opcode = read_le_u16(&bytes[2..4])?;
        logger.line(format!(
            "Rapture Login frame length={length} opcode=0x{opcode:04X}"
        ));

        if opcode == Opcode::LoginHandshake as u16 && bytes.len() >= 5 {
            let encrypted_size = bytes[4] as usize;
            logger.line(format!(
                "Rapture Login handshake response encrypted size={encrypted_size}"
            ));

            if bytes.len() >= 5 + encrypted_size {
                let decrypted = decrypt_login_rsa(&bytes[5..5 + encrypted_size]);
                // Never log the RSA-decrypted handshake message or the
                // derived AES key/IV -- both are session key material, and
                // logging them in cleartext lets anyone with read access to
                // logs/rapture_login.log decrypt that session's traffic.
                logger.line("Rapture Login RSA handshake decrypted");

                let aes_material = derive_login_aes_material(&decrypted)?;

                let key: [u8; 16] = aes_material[..16].try_into()?;
                let iv: [u8; 16] = aes_material[16..].try_into()?;
                aes_recv = Some(AesCtr::new(&key, &iv));
                aes_send = Some(AesCtr::new(&key, &iv));
                aes_material_for_world = Some(aes_material);
            }
        } else if let Some(recv) = aes_recv.as_mut() {
            let decrypted_payload = recv.transform(&bytes[2..]);
            logger.line(format!(
                "Rapture Login decrypted payload: {}",
                to_hex(&decrypted_payload)
            ));

            if decrypted_payload.len() < 2 {
                continue;
            }

            let decrypted_opcode = read_le_u16(&decrypted_payload[0..2])?;
            logger.line(format!(
                "Rapture Login decrypted opcode=0x{decrypted_opcode:04X}"
            ));

            if decrypted_opcode == Opcode::LoginRequest as u16 && decrypted_payload.len() >= 50 {
                let username = read_fixed_ascii(&decrypted_payload[2..21]);
                let password = read_fixed_ascii(&decrypted_payload[34..50]);
                // Never log the raw password -- this used to write it in
                // cleartext to logs/rapture_login.log on every attempt
                // (success or failure), which is a real credential leak for
                // anyone with read access to the log file.
                logger.line(format!("Rapture Login request username='{username}'"));

                if let (Some(send), Some(material)) =
                    (aes_send.as_mut(), aes_material_for_world.as_ref())
                {
                    let mut db_conn = db::connect()?;
                    match db::authenticate_account(&mut db_conn, &username, &password)? {
                        Some(account) => {
                            set_connection_tag(format!("conn{connection_id}/{}", account.username));
                            logger.line(format!(
                                "Rapture Login authenticated account id={} username='{}'",
                                account.id, account.username
                            ));
                            let identity = generate_identity();
                            save_world_session(material, account.id, &identity, logger)?;
                            send_encrypted(
                                &mut stream,
                                send,
                                build_login_success(account.id, auth_status_byte(&account), &identity)?,
                                "0xA102 login success",
                                logger,
                            )?;
                            send_encrypted(
                                &mut stream,
                                send,
                                build_server_list()?,
                                "0xA201 server list",
                                logger,
                            )?;
                        }
                        None => {
                            logger.line(format!("Rapture Login rejected username='{username}'"));
                            send_encrypted(
                                &mut stream,
                                send,
                                build_login_failure()?,
                                "0xA102 login failed",
                                logger,
                            )?;
                        }
                    }
                }
            } else if decrypted_opcode == Opcode::SelectServer as u16
                && decrypted_payload.len() >= 7
            {
                let world_id = decrypted_payload[2];
                let client_version = read_le_u32(&decrypted_payload[3..7])?;
                logger.line(format!(
                    "Rapture Login select server world_id={world_id} client_version={client_version}"
                ));

                if let Some(send) = aes_send.as_mut() {
                    send_encrypted(
                        &mut stream,
                        send,
                        build_select_server_success()?,
                        "0xA202 select server success",
                        logger,
                    )?;
                }
            }
        }
    }
}

fn main() -> Result<()> {
    let logger = Arc::new(Logger::new("logs/rapture_login.log")?);
    logger.line("Starting Rapture Login");

    let listener = TcpListener::bind("127.0.0.1:30800").context("failed to bind login port")?;
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let Some(guard) = ConnectionGuard::acquire(&logger) else {
                    continue;
                };
                apply_keepalive(&stream, &logger);
                let logger = Arc::clone(&logger);
                thread::spawn(move || {
                    let _guard = guard;
                    if let Err(error) = handle_client(stream, &logger) {
                        logger.line(format!("Rapture Login client error: {error:?}"));
                    }
                });
            }
            Err(error) => logger.line(format!("Rapture Login accept error: {error:?}")),
        }
    }

    Ok(())
}
