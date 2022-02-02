use anyhow::bail;
use clap::Parser;
use concordium::{p2p_client::P2pClient, Empty, GetAddressInfoRequest, PeersRequest};
use prometheus::{GaugeVec, Opts, Registry};
use serde_json::Value;
use std::time::Duration;
use tonic::{metadata::MetadataValue, transport::channel::Channel, Request};
use warp::{Filter, Rejection, Reply};
pub mod concordium {
    tonic::include_proto!("concordium");
}

#[derive(Clone)]
pub struct PrometheusStats {
    account_balance: GaugeVec,
    account_staked_amount: GaugeVec,
    account_not_staked_amount: GaugeVec,
    account_scheduled_amount: GaugeVec,
    node_uptime: GaugeVec,
    node_peers_average_latency: GaugeVec,
    node_peers_count: GaugeVec,
    registry: Registry,
}

impl PrometheusStats {
    pub fn new() -> Self {
        let instance = Self {
            account_balance: GaugeVec::new(
                Opts::new("account_balance", "Account balance"),
                &["address"],
            )
            .expect("metric can be created"),
            account_staked_amount: GaugeVec::new(
                Opts::new("account_staked_amount", "Account staked amount"),
                &["address"],
            )
            .expect("metric can be created"),
            account_scheduled_amount: GaugeVec::new(
                Opts::new("account_scheduled_amount", "Scheduled amount for account"),
                &["address"],
            )
            .expect("metric can be created"),
            account_not_staked_amount: GaugeVec::new(
                Opts::new("account_not_staked_amount", "Account not staked amount"),
                &["address"],
            )
            .expect("metric can be created"),
            node_uptime: GaugeVec::new(
                Opts::new("node_uptime", "Node uptime"),
                &["node", "version", "online", "baking_committee", "finalization_committee"],
            )
            .expect("metric can be created"),
            node_peers_average_latency: GaugeVec::new(
                Opts::new("node_peers_average_latency", "Node peers average latency"),
                &["node"],
            )
            .expect("metric can be created"),
            node_peers_count: GaugeVec::new(
                Opts::new("node_peers_count", "Node peer count"),
                &["node"],
            )
            .expect("metric can be created"),
            registry: Registry::new(),
        };
        instance
            .registry
            .register(Box::new(instance.account_balance.clone()))
            .expect("collector can be registered");
        instance
            .registry
            .register(Box::new(instance.account_staked_amount.clone()))
            .expect("collector can be registered");
        instance
            .registry
            .register(Box::new(instance.account_scheduled_amount.clone()))
            .expect("collector can be registered");
        instance
            .registry
            .register(Box::new(instance.account_not_staked_amount.clone()))
            .expect("collector can be registered");
        instance
            .registry
            .register(Box::new(instance.node_uptime.clone()))
            .expect("collector can be registered");
        instance
            .registry
            .register(Box::new(instance.node_peers_count.clone()))
            .expect("collector can be registered");
        instance
            .registry
            .register(Box::new(instance.node_peers_average_latency.clone()))
            .expect("collector can be registered");
        instance
    }
}

impl Default for PrometheusStats {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(long)]
    account: Vec<String>,

    #[clap(long)]
    node: Vec<String>,

    #[clap(long, default_value = "grpc://127.0.0.1:10000")]
    grpc_url: String,

    #[clap(long, default_value = "rpcadmin")]
    token: String,

    #[clap(long, default_value = "15")]
    scrape_interval: u64,

    #[clap(long, default_value = "9982")]
    listen_port: u64,

    #[clap(long, default_value = "127.0.0.1")]
    listen_address: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let stats = PrometheusStats::default();
    let metrics_route =
        warp::path!("metrics").and(with_stats(stats.clone())).and_then(metrics_handler);

    tokio::task::spawn(data_collector(
        stats,
        args.scrape_interval,
        args.grpc_url.clone(),
        args.token.clone(),
        args.account.clone(),
        args.node.clone(),
    ));

    warp::serve(metrics_route)
        .run(
            format!("{}:{}", args.listen_address, args.listen_port)
                .parse::<std::net::SocketAddr>()
                .expect("listen address correct"),
        )
        .await;

    Ok(())
}

fn with_stats(
    stats: PrometheusStats,
) -> impl Filter<Extract = (PrometheusStats,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || stats.clone())
}

async fn data_collector(
    stats: PrometheusStats,
    scrape_interval: u64,
    grpc_url: String,
    token: String,
    accounts: Vec<String>,
    nodes: Vec<String>,
) {
    let mut collect_interval = tokio::time::interval(Duration::from_secs(scrape_interval));
    loop {
        collect_interval.tick().await;
        for i in &accounts {
            match get_address_balance(&grpc_url, &token, i).await {
                Ok((balance, scheduled, staked)) => {
                    stats.account_balance.with_label_values(&[i]).set(balance);
                    stats.account_scheduled_amount.with_label_values(&[i]).set(scheduled);
                    stats.account_staked_amount.with_label_values(&[i]).set(staked);
                    stats.account_not_staked_amount.with_label_values(&[i]).set(balance - staked);
                }
                _ => eprintln!("Could not obtain data for address {}", i),
            }
        }
        for i in &nodes {
            match get_node_stats(i, &token).await {
                Ok((
                    version,
                    uptime,
                    average_latency,
                    peers_count,
                    baking_committee,
                    finalization_committee,
                )) => {
                    stats
                        .node_uptime
                        .with_label_values(&[
                            i,
                            &version,
                            "1",
                            &baking_committee.to_string(),
                            &finalization_committee.to_string(),
                        ])
                        .set(uptime as f64);
                    stats.node_peers_average_latency.with_label_values(&[i]).set(average_latency);
                    stats.node_peers_count.with_label_values(&[i]).set(peers_count as f64);
                }
                _ => {
                    stats.node_uptime.with_label_values(&[i, "N/A", "0", "0", "0"]).set(0.00);
                    stats.node_peers_average_latency.with_label_values(&[i]).set(0.00);
                    stats.node_peers_count.with_label_values(&[i]).set(0.00);
                    eprintln!("Could not obtain stats from host {}", i);
                }
            }
        }
    }
}

async fn get_address_balance(
    host: &str,
    token: &str,
    address: &str,
) -> anyhow::Result<(f64, f64, f64)> {
    let channel = Channel::from_shared(host.to_owned()).unwrap().connect().await?;
    let mut client = P2pClient::new(channel);

    // Query about consensus status and find last finalized height
    let node_consensus_status_reply = client.get_consensus_status(empty_req(token)).await?;
    let consensus_status_json: Value =
        serde_json::from_str(&node_consensus_status_reply.get_ref().value)?;

    // Query about account information
    let address_info_reply = client
        .get_account_info(get_address_info_req(
            token,
            address,
            consensus_status_json["lastFinalizedBlock"].as_str().unwrap(),
        ))
        .await?;
    let address_info_reply_json: Value = serde_json::from_str(&address_info_reply.get_ref().value)?;
    if address_info_reply_json.get("accountAmount").is_some() {
        let scheduled_for_release = match address_info_reply_json.get("accountReleaseSchedule") {
            Some(val) => match val.get("total").unwrap().as_str().unwrap().parse::<usize>() {
                Ok(num) => num,
                Err(e) => bail!(format!("{}", e)),
            },
            None => 0,
        };
        let account_balance =
            match address_info_reply_json["accountAmount"].as_str().unwrap().parse::<usize>() {
                Ok(num) => num,
                Err(e) => bail!(format!("{}", e)),
            };
        let account_staked = match address_info_reply_json.get("accountBaker") {
            Some(val) => {
                match val.get("stakedAmount").unwrap().as_str().unwrap().parse::<usize>() {
                    Ok(num) => num,
                    Err(e) => bail!(format!("{}", e)),
                }
            }
            None => 0,
        };
        Ok((
            account_balance as f64 / 1_000_000.00,
            scheduled_for_release as f64 / 1_000_000.00,
            account_staked as f64 / 1_000_000.00,
        ))
    } else {
        bail!("Account could not be looked up")
    }
}

async fn get_node_stats(
    host: &str,
    token: &str,
) -> anyhow::Result<(String, u64, f64, usize, usize, usize)> {
    let channel =
        Channel::from_shared(format!("grpc://{}", host.to_owned())).unwrap().connect().await?;
    let mut client = P2pClient::new(channel);

    // Query node for info
    let node_version_resp = client.peer_version(empty_req(token)).await?;
    let peer_stats_resp = client.peer_stats(get_peers_req(token)).await?;
    let node_info_resp = client.node_info(empty_req(token)).await?;
    let node_uptime_resp = client.peer_uptime(empty_req(token)).await?;

    let node_version = node_version_resp.get_ref().value.to_string();
    let node_uptime = node_uptime_resp.get_ref().value;

    let peers_average_latency = peer_stats_resp
        .get_ref()
        .peerstats
        .iter()
        .map(|element| element.latency)
        .sum::<u64>() as f64
        / peer_stats_resp.get_ref().peerstats.iter().filter(|element| element.latency > 0).count()
            as f64;
    let peers_count = peer_stats_resp.get_ref().peerstats.len();

    let baker_committee = match node_info_resp.get_ref().consensus_baker_committee() {
        concordium::node_info_response::IsInBakingCommittee::ActiveInCommittee => 1,
        _ => 0,
    };

    let finalization_committee = match node_info_resp.get_ref().consensus_finalizer_committee {
        true => 1,
        _ => 0,
    };

    Ok((
        node_version,
        node_uptime,
        peers_average_latency,
        peers_count,
        baker_committee,
        finalization_committee,
    ))
}

async fn metrics_handler(stats: PrometheusStats) -> Result<impl Reply, Rejection> {
    use prometheus::Encoder;
    let encoder = prometheus::TextEncoder::new();

    let mut buffer = Vec::new();
    if let Err(e) = encoder.encode(&stats.registry.gather(), &mut buffer) {
        eprintln!("could not encode custom metrics: {}", e);
    };
    let mut res = match String::from_utf8(buffer.clone()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("custom metrics could not be from_utf8'd: {}", e);
            String::default()
        }
    };
    buffer.clear();

    let mut buffer = Vec::new();
    if let Err(e) = encoder.encode(&prometheus::gather(), &mut buffer) {
        eprintln!("could not encode prometheus metrics: {}", e);
    };
    let res_custom = match String::from_utf8(buffer.clone()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("prometheus metrics could not be from_utf8'd: {}", e);
            String::default()
        }
    };
    buffer.clear();

    res.push_str(&res_custom);
    Ok(res)
}

fn empty_req(token: &str) -> Request<Empty> {
    let mut req = Request::new(Empty {});
    req.metadata_mut().insert("authentication", MetadataValue::from_str(token).unwrap());
    req
}

fn get_address_info_req(token: &str, address: &str, block: &str) -> Request<GetAddressInfoRequest> {
    let mut req = Request::new(GetAddressInfoRequest {
        address: address.to_owned(),
        block_hash: block.to_owned(),
    });
    req.metadata_mut().insert("authentication", MetadataValue::from_str(token).unwrap());
    req
}

fn get_peers_req(token: &str) -> Request<PeersRequest> {
    let mut req = Request::new(PeersRequest {
        include_bootstrappers: false,
    });
    req.metadata_mut().insert("authentication", MetadataValue::from_str(token).unwrap());
    req
}
