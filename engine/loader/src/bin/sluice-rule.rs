//! sluice-rule — a tiny gRPC client for the engine's decision RPCs (E3.1 diagnostic / test).
//!
//! Run as the owner (or root) while `sluice-engine` is up:
//!   sluice-rule list
//!   sluice-rule block <ip> [port]     # port omitted / 0 = any port to that IP
//!   sluice-rule remove <id>           # id from `list`, e.g. v4:1.2.3.4:443
//!   sluice-rule inbound               # show inbound posture
//!   sluice-rule inbound-set on|off [tcp:PORT|udp:PORT ...]   # set inbound policy

use sluice_proto::{
    sluice_engine_client::SluiceEngineClient, InboundAllow, InboundPolicy, InboundQuery,
    ListRequest, Rule, RuleId,
};
use tonic::transport::{Endpoint, Uri};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let uds = std::env::var("SLUICE_ENGINE_UDS")
        .unwrap_or_else(|_| "/run/sluice/engine.sock".to_string());
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("list");

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

    match cmd {
        "block" => {
            let ip = args.get(2).cloned().unwrap_or_default();
            let port: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
            let ack = client
                .set_rule(Rule {
                    id: String::new(),
                    action: 1,
                    dst_ip: ip.clone(),
                    dst_port: port,
                })
                .await?
                .into_inner();
            println!("block {ip}:{port} -> ok={} {}", ack.ok, ack.error);
            if !ack.ok {
                std::process::exit(1);
            }
        }
        "remove" => {
            let id = args.get(2).cloned().unwrap_or_default();
            let ack = client
                .remove_rule(RuleId { id: id.clone() })
                .await?
                .into_inner();
            println!("remove {id} -> ok={} {}", ack.ok, ack.error);
            if !ack.ok {
                std::process::exit(1);
            }
        }
        "inbound" => {
            let p = client
                .get_inbound_policy(InboundQuery {})
                .await?
                .into_inner();
            println!(
                "inbound: {}",
                if p.enforce { "ENFORCING" } else { "observe" }
            );
            for a in p.allow {
                println!("  allow {}:{}", a.proto, a.port);
            }
        }
        "inbound-set" => {
            let enforce = args.get(2).map(|s| s == "on").unwrap_or(false);
            let allow: Vec<InboundAllow> = args[3.min(args.len())..]
                .iter()
                .filter_map(|s| {
                    let (proto, port) = s.split_once(':')?;
                    Some(InboundAllow {
                        proto: proto.to_string(),
                        port: port.parse().ok()?,
                    })
                })
                .collect();
            let ack = client
                .set_inbound_policy(InboundPolicy { enforce, allow })
                .await?
                .into_inner();
            println!(
                "inbound-set enforce={enforce} -> ok={} {}",
                ack.ok, ack.error
            );
            if !ack.ok {
                std::process::exit(1);
            }
        }
        _ => {
            let list = client.list_rules(ListRequest {}).await?.into_inner();
            if list.rules.is_empty() {
                println!("(no rules)");
            }
            for r in list.rules {
                println!("{}  block {}:{}", r.id, r.dst_ip, r.dst_port);
            }
        }
    }
    Ok(())
}
