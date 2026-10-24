//! Tunnel lifecycle: pick a server, point macOS's tunnel at it, run WireGuard on the utun,
//! confirm a handshake, and keep watching it.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use gotatun::device::{Device, DeviceBuilder, Peer};
use gotatun::udp::socket::UdpSocketFactory;
use gotatun::x25519::{PublicKey, StaticSecret};
use shared::{Request, Settings, Status, net};
use tokio::runtime::Runtime;

use crate::provider::{Done, Handle, Reply, Stopped};
use crate::utun::{self, Utun};

type Dev = Device<(UdpSocketFactory, Utun, Utun)>;

/// Servers to try per start before giving up.
const ATTEMPTS: usize = 3;
/// How long a server gets to answer the first handshake.
const HANDSHAKE_WAIT: Duration = Duration::from_secs(8);
/// With a 25 s keepalive, WireGuard re-handshakes every 2 minutes. Older than this: nudge it.
const STALE: Duration = Duration::from_secs(180);
/// Older than this: give up on the server and let macOS restart the tunnel.
const DEAD: Duration = Duration::from_secs(360);

fn rt() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("tunnel")
            .enable_all()
            .build()
            .expect("tokio runtime")
    })
}

static DEVICE: tokio::sync::Mutex<Option<Dev>> = tokio::sync::Mutex::const_new(None);

struct Info {
    server: Option<nord::Server>,
    since: Option<u64>,
    problem: Option<String>,
    /// Bumped on every start and stop, so an old watchdog knows to quit.
    generation: u64,
}

static INFO: Mutex<Info> = Mutex::new(Info { server: None, since: None, problem: None, generation: 0 });

fn bump() -> u64 {
    let mut info = INFO.lock().unwrap();
    info.generation += 1;
    info.generation
}

pub fn start(provider: Handle, key_from_app: Option<String>, settings: Settings, done: Done) {
    let generation = bump();
    rt().spawn(async move {
        match connect(&provider, key_from_app, &settings).await {
            Ok(server) => {
                log::info!("connected to {} in {}", server.hostname, server.city);
                let mut info = INFO.lock().unwrap();
                info.server = Some(server);
                info.since = Some(SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()));
                info.problem = None;
                drop(info);
                done.finish(None);
                tokio::spawn(watchdog(provider, generation));
            }
            Err(e) => {
                log::error!("couldn't connect: {e}");
                stop_device().await;
                INFO.lock().unwrap().problem = Some(e.clone());
                done.finish(Some(&e));
            }
        }
    });
}

pub fn stop(done: Stopped) {
    bump();
    rt().spawn(async move {
        stop_device().await;
        let mut info = INFO.lock().unwrap();
        info.server = None;
        info.since = None;
        drop(info);
        done.finish();
    });
}

pub fn wake() {
    rt().spawn(async {
        log::info!("woke from sleep; renewing the handshake");
        force_handshake().await;
    });
}

pub fn message(request: Option<Request>, reply: Option<Reply>) {
    rt().spawn(async move {
        let bytes = match request {
            Some(Request::Status) => shared::encode(&status().await),
            Some(Request::SetKey { key }) if nord::is_wireguard_key(&key) => {
                let mut store = crate::store::load();
                store.private_key = Some(key);
                crate::store::save(&store);
                log::info!("private key updated by the app");
                shared::encode(&status().await)
            }
            _ => Vec::new(),
        };
        if let Some(reply) = reply {
            reply.send(&bytes);
        }
    });
}

async fn status() -> Status {
    let (server, since, problem) = {
        let info = INFO.lock().unwrap();
        (info.server.clone(), info.since, info.problem.clone())
    };
    let mut status = Status {
        server: server.as_ref().map(|s| s.name.clone()),
        hostname: server.as_ref().map(|s| s.hostname.clone()),
        country: server.as_ref().map(|s| s.country.clone()),
        country_code: server.as_ref().map(|s| s.country_code.clone()),
        city: server.as_ref().map(|s| s.city.clone()),
        connected_since: since,
        problem,
        ..Status::default()
    };
    if let Some(device) = DEVICE.lock().await.as_ref()
        && let Some(peer) = device.peers().await.first()
    {
        status.last_handshake_secs = peer.stats.last_handshake.map(|d| d.as_secs());
        status.rx_bytes = peer.stats.rx_bytes as u64;
        status.tx_bytes = peer.stats.tx_bytes as u64;
    }
    status
}

fn decode_key(b64: &str) -> Result<[u8; 32], String> {
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .ok()
        .and_then(|b| <[u8; 32]>::try_from(b).ok())
        .ok_or_else(|| "The stored WireGuard key is damaged. Open Kivuko and connect again.".to_string())
}

async fn connect(provider: &Handle, key_from_app: Option<String>, settings: &Settings) -> Result<nord::Server, String> {
    stop_device().await;
    let mut store = crate::store::load();
    if let Some(key) = key_from_app.filter(|k| nord::is_wireguard_key(k))
        && store.private_key.as_deref() != Some(key.as_str())
    {
        store.private_key = Some(key);
        crate::store::save(&store);
    }
    let key = store
        .private_key
        .clone()
        .ok_or("No WireGuard key yet. Open Kivuko and click Connect once.")?;
    let secret = StaticSecret::from(decode_key(&key)?);
    let servers = pick_servers(settings, &mut store).await?;

    for server in servers.iter().take(ATTEMPTS) {
        let Some(endpoint) = server.endpoint() else { continue };
        provider
            .apply(server, settings.dns)
            .await
            .map_err(|_| "macOS didn't answer while setting up the tunnel.".to_string())?
            .map_err(|e| format!("macOS refused the tunnel settings: {e}"))?;
        let fd = utun::find_fd().ok_or("macOS didn't create the tunnel interface.")?;
        let public = PublicKey::from(decode_key(&server.public_key).map_err(|_| "bad server key")?);
        let mut peer = Peer::new(public).with_endpoint(endpoint).with_allowed_ips([
            "0.0.0.0/0".parse().expect("IPv4 default route"),
            "::/0".parse().expect("IPv6 default route"),
        ]);
        peer.keepalive = Some(net::KEEPALIVE_SECS);
        use_peer(fd, &secret, peer).await?;
        log::info!("trying {} ({}% load)", server.hostname, server.load);
        if wait_for_handshake(HANDSHAKE_WAIT).await {
            return Ok(server.clone());
        }
        log::warn!("no handshake from {}", server.hostname);
    }
    Err("No NordVPN server answered. Check your internet connection; if it's fine, your Nord token may have been revoked.".into())
}

/// Fresh recommendations when Nord's API answers; the last good list when it doesn't.
async fn pick_servers(settings: &Settings, store: &mut crate::store::Store) -> Result<Vec<nord::Server>, String> {
    let country = settings.country_id;
    let fresh = tokio::task::spawn_blocking(move || nord::Client::new(Duration::from_secs(8)).recommendations(country, 5)).await;
    match fresh {
        Ok(Ok(list)) => {
            store.servers = list.clone();
            crate::store::save(store);
            Ok(list)
        }
        other => {
            let why = match other {
                Ok(Err(e)) => e.to_string(),
                _ => "lookup failed".into(),
            };
            let cached: Vec<_> = store
                .servers
                .iter()
                .filter(|s| country.is_none_or(|id| s.country_id == id))
                .cloned()
                .collect();
            if cached.is_empty() {
                Err(format!("Couldn't pick a server: {why}."))
            } else {
                log::warn!("using saved servers: {why}");
                Ok(cached)
            }
        }
    }
}

/// Start WireGuard on the utun with this peer, or swap the peer if it's already running.
async fn use_peer(fd: i32, secret: &StaticSecret, peer: Peer) -> Result<(), String> {
    let mut device = DEVICE.lock().await;
    if let Some(d) = device.as_ref() {
        d.clear_peers().await.map_err(|e| e.to_string())?;
        d.add_peer(peer).await.map_err(|e| e.to_string())?;
        return Ok(());
    }
    let utun = Utun::new(fd, net::MTU).map_err(|e| format!("couldn't use the tunnel interface: {e}"))?;
    let d = DeviceBuilder::new()
        .with_default_udp()
        .with_ip(utun)
        .with_private_key(secret.clone())
        .with_peer(peer)
        .build()
        .await
        .map_err(|e| format!("WireGuard didn't start: {e}"))?;
    *device = Some(d);
    Ok(())
}

async fn handshake_age() -> Option<Duration> {
    let device = DEVICE.lock().await;
    let peers = device.as_ref()?.peers().await;
    peers.first().and_then(|p| p.stats.last_handshake)
}

async fn wait_for_handshake(timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if handshake_age().await.is_some() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    false
}

async fn force_handshake() {
    let device = DEVICE.lock().await;
    if let Some(d) = device.as_ref() {
        for p in d.peers().await {
            let _ = d.force_handshake(&p.peer.public_key).await;
        }
    }
}

async fn stop_device() {
    let device = DEVICE.lock().await.take();
    if let Some(d) = device {
        d.stop().await;
    }
}

/// Every 15 s: nudge a stale handshake; give up on a dead server so macOS restarts the
/// tunnel (On-Demand) with a fresh pick.
async fn watchdog(provider: Handle, generation: u64) {
    let mut tick = tokio::time::interval(Duration::from_secs(15));
    let mut nudged = false;
    loop {
        tick.tick().await;
        if INFO.lock().unwrap().generation != generation {
            return;
        }
        match handshake_age().await {
            Some(age) if age > DEAD => {
                log::warn!("no handshake for {}s; restarting the tunnel", age.as_secs());
                INFO.lock().unwrap().problem = Some("Lost the connection to the VPN server.".into());
                provider.cancel("Lost the connection to the VPN server.");
                return;
            }
            Some(age) if age > STALE => {
                if !nudged {
                    log::info!("handshake is {}s old; renewing it", age.as_secs());
                    provider.set_reasserting(true);
                    nudged = true;
                }
                force_handshake().await;
            }
            Some(_) if nudged => {
                provider.set_reasserting(false);
                nudged = false;
            }
            _ => {}
        }
    }
}
