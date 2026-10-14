//! "Anyone, over the internet": a free Cloudflare quick tunnel gives the
//! hosting server a public `https://….trycloudflare.com` address, with no
//! account and no router setup. `cloudflared` isn't bundled; the Host panel
//! asks people to install it and picks it up as soon as it's there.

use serde::Serialize;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase", tag = "state")]
pub enum Phase {
    #[default]
    Off,
    /// `cloudflared` isn't installed.
    Missing,
    Starting,
    Ready { url: String },
    Failed { error: String },
}

#[derive(Default)]
struct Inner {
    child: Option<Child>,
    port: u16,
    /// Bumped on every start/stop so a finished reader can tell it's stale.
    run: u64,
    phase: Phase,
}

#[derive(Default)]
pub struct Tunnel(Arc<Mutex<Inner>>);

fn lock(m: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Tunnel {
    pub fn phase(&self) -> Phase {
        lock(&self.0).phase.clone()
    }

    /// The public address, once Cloudflare is routing to it.
    pub fn url(&self) -> Option<String> {
        match self.phase() {
            Phase::Ready { url } => Some(url),
            _ => None,
        }
    }

    /// Makes sure a tunnel to `port` is up or on its way. A failed tunnel is
    /// only retried when `retry` is set, so polling doesn't hammer Cloudflare.
    pub fn ensure(&self, port: u16, retry: bool, pid_file: &Path) {
        let mut inner = lock(&self.0);
        let alive = inner.child.is_some() && inner.port == port;
        match inner.phase {
            Phase::Starting | Phase::Ready { .. } if alive => return,
            Phase::Failed { .. } if !retry => return,
            _ => {}
        }
        kill(&mut inner, pid_file);
        let Some(exe) = find_cloudflared() else {
            inner.phase = Phase::Missing;
            return;
        };
        kill_stale(pid_file);
        let spawned = Command::new(exe)
            .args(["tunnel", "--no-autoupdate", "--url", &format!("http://127.0.0.1:{port}")])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(e) => {
                inner.phase = Phase::Failed { error: format!("Couldn’t start cloudflared: {e}") };
                return;
            }
        };
        let _ = std::fs::write(pid_file, child.id().to_string());
        let stderr = child.stderr.take().expect("piped stderr");
        inner.run += 1;
        inner.child = Some(child);
        inner.port = port;
        inner.phase = Phase::Starting;

        let run = inner.run;
        let shared = self.0.clone();
        std::thread::spawn(move || watch(BufReader::new(stderr), run, &shared));
    }

    pub fn stop(&self, pid_file: &Path) {
        kill(&mut lock(&self.0), pid_file);
    }
}

fn kill(inner: &mut Inner, pid_file: &Path) {
    inner.run += 1;
    inner.phase = Phase::Off;
    if let Some(mut child) = inner.child.take() {
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(pid_file);
    }
}

/// Reads cloudflared's log: the address shows up first, and it's reachable
/// once a connection to Cloudflare is registered. Handing it out any earlier
/// risks guests caching a "no such host" answer for minutes.
fn watch(log: impl BufRead, run: u64, shared: &Mutex<Inner>) {
    let mut url = None;
    let mut last_error = None;
    for line in log.lines().map_while(std::result::Result::ok) {
        if url.is_none() {
            url = quick_tunnel_url(&line);
        }
        if line.contains(" ERR ") {
            last_error = line.split_once(" ERR ").map(|(_, msg)| msg.trim().to_owned());
        }
        if line.contains("Registered tunnel connection") {
            if let Some(url) = &url {
                let mut inner = lock(shared);
                if inner.run == run {
                    inner.phase = Phase::Ready { url: url.clone() };
                }
            }
        }
    }
    // cloudflared exited.
    let mut inner = lock(shared);
    if inner.run == run {
        let error = match last_error {
            Some(e) if e.contains("quick Tunnel") => {
                "Couldn’t get an address from Cloudflare. Check your internet connection and try again.".to_owned()
            }
            Some(e) => format!("The Cloudflare connection stopped: {e}"),
            None => "The Cloudflare connection stopped.".to_owned(),
        };
        inner.phase = Phase::Failed { error };
        if let Some(mut child) = inner.child.take() {
            let _ = child.wait();
        }
    }
}

fn quick_tunnel_url(line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let url: String = line[start..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | ':' | '/'))
        .collect();
    url.ends_with(".trycloudflare.com").then_some(url)
}

/// A tunnel left behind by a crash would keep pointing at this port.
fn kill_stale(pid_file: &Path) {
    let Some(pid) = std::fs::read_to_string(pid_file).ok().and_then(|s| s.trim().parse::<u32>().ok()) else {
        return;
    };
    let _ = std::fs::remove_file(pid_file);
    let comm = Command::new("ps").args(["-p", &pid.to_string(), "-o", "comm="]).output();
    if comm.is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim().ends_with("cloudflared")) {
        let _ = Command::new("kill").arg(pid.to_string()).status();
    }
}

/// Apps opened from the Dock don't get the shell's PATH, so also look where
/// Homebrew and the install command put it.
fn search_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"].map(PathBuf::from));
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(Path::new(&home).join(".local/bin"));
    }
    dirs
}

pub fn find_cloudflared() -> Option<PathBuf> {
    search_dirs().into_iter().map(|d| d.join("cloudflared")).find(|p| p.is_file())
}

/// A one-line Terminal command that installs cloudflared.
pub fn install_command() -> String {
    if search_dirs().iter().any(|d| d.join("brew").is_file()) {
        return "brew install cloudflared".into();
    }
    let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "amd64" };
    format!(
        "mkdir -p ~/.local/bin && curl -fsSL https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-darwin-{arch}.tgz | tar xz -C ~/.local/bin"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_address_in_the_banner() {
        let line = "2026-10-13T22:45:03Z INF |  https://axis-vocals-diabetes-score.trycloudflare.com        |";
        assert_eq!(quick_tunnel_url(line).as_deref(), Some("https://axis-vocals-diabetes-score.trycloudflare.com"));
        assert_eq!(quick_tunnel_url("INF Requesting new quick Tunnel on trycloudflare.com..."), None);
        assert_eq!(quick_tunnel_url("INF see https://developers.cloudflare.com/x"), None);
    }
}

#[cfg(test)]
mod live {
    use super::*;

    /// Opens a real quick tunnel to TUNNEL_TEST_PORT and keeps it up until
    /// TUNNEL_TEST_DIR/stop appears. Needs cloudflared and the internet.
    #[test]
    #[ignore]
    fn quick_tunnel() {
        let port: u16 = std::env::var("TUNNEL_TEST_PORT").unwrap().parse().unwrap();
        let dir = PathBuf::from(std::env::var("TUNNEL_TEST_DIR").unwrap());
        let tunnel = Tunnel::default();
        let pid_file = dir.join("cloudflared.pid");
        tunnel.ensure(port, false, &pid_file);
        for _ in 0..60 {
            if tunnel.url().is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        let url = tunnel.url().expect("tunnel ready");
        std::fs::write(dir.join("url"), &url).unwrap();
        while !dir.join("stop").exists() {
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
        tunnel.stop(&pid_file);
        assert!(!pid_file.exists());
    }
}
