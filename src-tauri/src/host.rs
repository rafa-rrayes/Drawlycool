//! "Host on this Mac": runs the sync server inside the app so other people can
//! open the canvases in `<data>/hosted/`.

use crate::network::{HostConfig, Net};
use drawlycool_server::{new_token, Config, Server};
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{AppHandle, Manager, State};

type Result<T> = std::result::Result<T, String>;

static RUNNING: AtomicBool = AtomicBool::new(false);

#[derive(Default)]
pub struct HostState {
    server: tokio::sync::Mutex<Option<Server>>,
    error: std::sync::Mutex<Option<String>>,
}

pub fn is_running() -> bool {
    RUNNING.load(Ordering::SeqCst)
}

#[derive(Serialize, Clone)]
pub struct Address {
    label: String,
    address: String,
}

/// Ways other machines can reach this Mac, best first.
pub fn addresses() -> Vec<Address> {
    let mut list = Vec::new();
    if let Some(name) = local_host_name() {
        list.push(Address { label: "Local network (Bonjour name)".into(), address: format!("{name}.local") });
    }
    let mut ips: Vec<Address> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.is_loopback())
        .filter_map(|i| match i.ip() {
            IpAddr::V4(ip) if !ip.is_link_local() => Some(ip),
            _ => None,
        })
        .map(|ip| {
            let o = ip.octets();
            let label = if o[0] == 100 && (64..128).contains(&o[1]) {
                "Tailscale"
            } else if ip.is_private() {
                "Local network"
            } else {
                "Public address"
            };
            Address { label: label.into(), address: ip.to_string() }
        })
        .collect();
    ips.sort_by_key(|a| match a.label.as_str() {
        "Local network" => 0,
        "Tailscale" => 1,
        _ => 2,
    });
    ips.dedup_by(|a, b| a.address == b.address);
    list.extend(ips);
    list
}

fn local_host_name() -> Option<String> {
    let out = std::process::Command::new("scutil").args(["--get", "LocalHostName"]).output().ok()?;
    let name = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (!name.is_empty()).then_some(name)
}

/// The URL other people use to reach this Mac.
pub fn invite_url(config: &HostConfig) -> String {
    let address = config
        .invite_address
        .clone()
        .filter(|a| !a.is_empty())
        .or_else(|| addresses().into_iter().next().map(|a| a.address))
        .unwrap_or_else(|| "localhost".into());
    format!("http://{address}:{}", config.port)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostStatus {
    enabled: bool,
    running: bool,
    port: u16,
    name: String,
    invite_address: Option<String>,
    invite_url: String,
    addresses: Vec<Address>,
    error: Option<String>,
}

fn status(net: &Net, host: &HostState) -> HostStatus {
    let c = net.config().host;
    HostStatus {
        enabled: c.enabled,
        running: is_running(),
        port: c.port,
        name: c.name.clone(),
        invite_address: c.invite_address.clone(),
        invite_url: invite_url(&c),
        addresses: addresses(),
        error: host.error.lock().unwrap_or_else(|e| e.into_inner()).clone(),
    }
}

async fn start(app: &AppHandle) -> Result<()> {
    let net = app.state::<Net>();
    let host = app.state::<HostState>();
    let mut slot = host.server.lock().await;
    if slot.is_some() {
        return Ok(());
    }
    // First run: pick a token and a name.
    let config = net
        .update_config(|c| {
            if c.host.token.is_empty() {
                c.host.token = new_token();
            }
            if c.host.name.trim().is_empty() {
                let who = crate::network::user_full_name();
                let first = who.split_whitespace().next().unwrap_or("My").to_owned();
                c.host.name = format!("{first}’s Mac");
            }
        })?
        .host;
    let server = Server::start(Config {
        data_dir: net.data_dir.join("hosted"),
        addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), config.port),
        token: config.token.clone(),
        name: config.name.clone(),
    })
    .await;
    match server {
        Ok(server) => {
            *slot = Some(server);
            RUNNING.store(true, Ordering::SeqCst);
            *host.error.lock().unwrap_or_else(|e| e.into_inner()) = None;
            Ok(())
        }
        Err(e) => {
            let msg = if e.kind() == std::io::ErrorKind::AddrInUse {
                format!("Port {} is already in use. Pick another one.", config.port)
            } else {
                format!("Couldn’t start hosting: {e}")
            };
            *host.error.lock().unwrap_or_else(|e| e.into_inner()) = Some(msg.clone());
            Err(msg)
        }
    }
}

/// Stops hosting (saving everything first). Safe to call when not hosting.
pub async fn stop(app: &AppHandle) {
    let host = app.state::<HostState>();
    let server = host.server.lock().await.take();
    RUNNING.store(false, Ordering::SeqCst);
    if let Some(server) = server {
        server.stop().await;
    }
}

/// Starts hosting at launch if it was on last time.
pub fn restore(app: &AppHandle) {
    if !app.state::<Net>().config().host.enabled {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = start(&app).await;
    });
}

#[tauri::command]
pub fn host_status(net: State<Net>, host: State<HostState>) -> HostStatus {
    status(&net, &host)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostPatch {
    enabled: Option<bool>,
    port: Option<u16>,
    name: Option<String>,
    invite_address: Option<String>,
    /// Issue a new access token; existing invite links stop working.
    #[serde(default)]
    reset_token: bool,
}

#[tauri::command]
pub async fn host_update(app: AppHandle, patch: HostPatch) -> Result<HostStatus> {
    let net = app.state::<Net>();
    let before = net.config().host;
    let after = net
        .update_config(|c| {
            if let Some(v) = patch.enabled {
                c.host.enabled = v;
            }
            if let Some(p) = patch.port.filter(|p| *p >= 1024) {
                c.host.port = p;
            }
            if let Some(n) = patch.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
                c.host.name = n.chars().take(60).collect();
            }
            if patch.reset_token {
                c.host.token = new_token();
            }
            if let Some(a) = patch.invite_address.as_deref() {
                c.host.invite_address = Some(a.trim().to_owned()).filter(|a| !a.is_empty());
            }
        })?
        .host;

    // Restart for anything the running server was started with.
    let needs_restart = before.port != after.port || before.name != after.name || before.token != after.token;
    if !after.enabled || needs_restart {
        stop(&app).await;
    }
    if after.enabled {
        // A failure (e.g. port in use) is reported through `status().error`;
        // hosting stays "enabled" so it can be retried.
        let _ = start(&app).await;
    } else {
        *app.state::<HostState>().error.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
    Ok(status(&net, &app.state::<HostState>()))
}
