//! sluice-watch — a tiny unprivileged gRPC client for the engine's connection stream.
//!
//! Diagnostic + E3.0 verification without the GUI: connect to the running `sluice-engine` over
//! its UDS and print each observed connection. Run as the owner (or root) while the engine is
//! up: `SLUICE_ENGINE_UDS=/run/sluice/engine.sock sluice-watch`.

use sluice_proto::{sluice_engine_client::SluiceEngineClient, WatchRequest};
use tonic::transport::{Endpoint, Uri};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let uds = std::env::var("SLUICE_ENGINE_UDS")
        .unwrap_or_else(|_| "/run/sluice/engine.sock".to_string());
    eprintln!("[watch] connecting to {uds}");

    // Custom connector: the URI is a placeholder; we always dial the UDS path.
    let path = uds.clone();
    let connector = tower::service_fn(move |_: Uri| {
        let path = path.clone();
        async move {
            let stream = tokio::net::UnixStream::connect(&path).await?;
            Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
        }
    });
    let channel = Endpoint::try_from("http://127.0.0.1:50051")?
        .connect_with_connector(connector)
        .await?;

    let mut client = SluiceEngineClient::new(channel);
    let mut stream = client
        .watch_connections(WatchRequest {})
        .await?
        .into_inner();
    eprintln!("[watch] streaming connection events (Ctrl-C to stop)…");

    let mut n: u64 = 0;
    while let Some(ev) = stream.message().await? {
        n += 1;
        let tag = if ev.inbound {
            "IN  "
        } else if ev.verdict == 1 {
            "BLOCK"
        } else {
            "allow"
        };
        let (pid, uid) = (ev.pid, ev.uid);
        let proc = if !ev.process_path.is_empty() {
            ev.process_path
        } else {
            ev.comm
        };
        // Show the resolved hostname (E4) when the DNS snoop has it, else just the IP.
        let dst = if ev.dst_host.is_empty() {
            format!("{}:{}", ev.dst_ip, ev.dst_port)
        } else {
            format!("{} [{}]:{}", ev.dst_host, ev.dst_ip, ev.dst_port)
        };
        let ctr = if ev.container.is_empty() {
            String::new()
        } else {
            format!(" 🐳 {}", ev.container)
        };
        println!("#{n:<5} {tag:<5} pid={pid:<7} uid={uid:<6} {dst}  [{proc}]{ctr}");
    }
    Ok(())
}
