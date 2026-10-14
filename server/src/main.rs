//! Standalone sync server: `drawlycool-server --data ./canvases --port 4180`.

use drawlycool_server::{invite_link, new_token, Config, Server};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

const USAGE: &str = "\
Usage: drawlycool-server [options]

Options:
  --data <dir>      Where canvases are stored (default: ./drawlycool-data)
  --port <port>     Port to listen on (default: 4180)
  --host <ip>       Address to bind (default: 0.0.0.0)
  --name <name>     Server name shown in the app (default: Drawlycool server)
  --public-url <u>  URL people use to reach this server, for the invite link
                    (e.g. https://draw.example.com behind a TLS proxy)

The access token is read from DRAWLYCOOL_TOKEN, or generated once and kept in
<data>/token. DRAWLYCOOL_NAME and DRAWLYCOOL_PUBLIC_URL can stand in for --name
and --public-url (handy in containers).";

fn main() {
    let mut data = PathBuf::from("drawlycool-data");
    let mut port: u16 = 4180;
    let mut host: IpAddr = [0, 0, 0, 0].into();
    let env = |key: &str| std::env::var(key).ok().map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
    let mut name = env("DRAWLYCOOL_NAME").unwrap_or_else(|| "Drawlycool server".to_owned());
    let mut public_url: Option<String> = env("DRAWLYCOOL_PUBLIC_URL").map(|u| u.trim_end_matches('/').to_owned());

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| fail(&format!("{arg} needs a value")));
        match arg.as_str() {
            "--data" => data = PathBuf::from(value()),
            "--port" => port = value().parse().unwrap_or_else(|_| fail("invalid --port")),
            "--host" => host = value().parse().unwrap_or_else(|_| fail("invalid --host")),
            "--name" => name = value(),
            "--public-url" => public_url = Some(value().trim_end_matches('/').to_owned()),
            "-h" | "--help" => {
                println!("{USAGE}");
                return;
            }
            other => fail(&format!("unknown option {other}")),
        }
    }

    let token = std::env::var("DRAWLYCOOL_TOKEN").ok().filter(|t| !t.is_empty()).unwrap_or_else(|| {
        let path = data.join("token");
        if let Ok(t) = std::fs::read_to_string(&path) {
            return t.trim().to_owned();
        }
        let t = new_token();
        std::fs::create_dir_all(&data).and_then(|_| std::fs::write(&path, &t)).unwrap_or_else(|e| fail(&e.to_string()));
        t
    });

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let addr = SocketAddr::new(host, port);
        let config = Config { data_dir: data.clone(), addr, token: token.clone(), name };
        let server = Server::start(config).await.unwrap_or_else(|e| fail(&format!("can't listen on {addr}: {e}")));

        let url = public_url.unwrap_or_else(|| {
            let shown = if host.is_unspecified() { "<this-machine's-address>".to_owned() } else { host.to_string() };
            format!("http://{shown}:{}", server.local_addr.port())
        });
        println!("Drawlycool server listening on {}", server.local_addr);
        println!("Canvases: {}", data.display());
        println!("Invite link: {}", invite_link(&url, &token, None));

        shutdown_signal().await;
        println!("Saving and stopping…");
        server.stop().await;
    });
}

/// Ctrl-C, or SIGTERM from `docker stop` / systemd.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}

fn fail(msg: &str) -> ! {
    eprintln!("drawlycool-server: {msg}\n\n{USAGE}");
    std::process::exit(2);
}
