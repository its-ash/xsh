//! `adb-find` scans the local network for Android devices with ADB over
//! Wi-Fi; `adb-connect` connects to one of the devices it found.

use std::collections::BTreeSet;
use std::io::{self, BufRead as _, Write as _};
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use tokio::net::TcpStream;
use tokio::process::Command;
use tokio::task::JoinSet;
use tokio::time::timeout;

/// Port of `adb tcpip 5555`.
const PORT: u16 = 5555;
/// Parallel TCP scan workers; `adb mdns services` runs next to them.
const WORKERS: u8 = 4;
const CONNECT_TIMEOUT: Duration = Duration::from_millis(400);
const MDNS_TIMEOUT: Duration = Duration::from_secs(5);
const CACHE: &str = "adb-devices.txt";

fn cache_path(data_dir: &Path) -> PathBuf {
    data_dir.join(CACHE)
}

/// The IPv4 address of the interface that holds the default route.
fn local_ip() -> Result<Ipv4Addr> {
    let sock = UdpSocket::bind("0.0.0.0:0").context("cannot open a UDP socket")?;
    sock.connect("192.0.2.1:9")
        .context("no network route found")?;
    match sock.local_addr()?.ip() {
        std::net::IpAddr::V4(ip) if !ip.is_loopback() && !ip.is_unspecified() => Ok(ip),
        _ => bail!("no IPv4 network address found"),
    }
}

/// Hosts of the /24 network that `ip` is in, except `ip` itself.
fn hosts(ip: Ipv4Addr) -> Vec<Ipv4Addr> {
    let [a, b, c, own] = ip.octets();
    (1..=254u8)
        .filter(|&d| d != own)
        .map(|d| Ipv4Addr::new(a, b, c, d))
        .collect()
}

async fn probe(ip: Ipv4Addr) -> Option<String> {
    let addr = SocketAddr::from((ip, PORT));
    matches!(
        timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await,
        Ok(Ok(_))
    )
    .then(|| addr.to_string())
}

/// Wireless-debugging devices (Android 11+) that `adb` finds with mDNS.
async fn mdns() -> Vec<String> {
    let run = Command::new("adb")
        .args(["mdns", "services"])
        .kill_on_drop(true)
        .output();
    let Ok(Ok(out)) = timeout(MDNS_TIMEOUT, run).await else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains("_adb-tls-connect"))
        .filter_map(|l| l.split_whitespace().last())
        .filter(|a| a.parse::<SocketAddr>().is_ok())
        .map(str::to_owned)
        .collect()
}

/// Scans the network, prints the devices, and saves them for `adb-connect`.
pub async fn find(data_dir: &Path) -> Result<i32> {
    let ip = local_ip()?;
    let all = hosts(ip);
    println!(
        "Scanning {}.0/24 port {PORT} with {WORKERS} workers and adb mDNS...",
        Ipv4Addr::from(u32::from(ip) & 0xFFFF_FF00)
    );
    let mut set: JoinSet<Vec<String>> = JoinSet::new();
    for w in 0..WORKERS {
        let shard: Vec<Ipv4Addr> = all
            .iter()
            .skip(usize::from(w))
            .step_by(usize::from(WORKERS))
            .copied()
            .collect();
        set.spawn(async move {
            let mut res = Vec::new();
            for chunk in shard.chunks(16) {
                let mut probes: JoinSet<Option<String>> = JoinSet::new();
                for &h in chunk {
                    probes.spawn(probe(h));
                }
                while let Some(r) = probes.join_next().await {
                    res.extend(r.ok().flatten());
                }
            }
            res
        });
    }
    set.spawn(mdns());
    let mut found = BTreeSet::new();
    while let Some(r) = set.join_next().await {
        found.extend(r.unwrap_or_default());
    }
    if found.is_empty() {
        println!("No ADB devices found. Enable `adb tcpip {PORT}` or wireless debugging.");
    } else {
        for (i, d) in found.iter().enumerate() {
            println!("  {}) {d}", i + 1);
        }
    }
    std::fs::create_dir_all(data_dir)?;
    let text: Vec<&str> = found.iter().map(String::as_str).collect();
    std::fs::write(cache_path(data_dir), text.join("\n")).context("cannot save the scan result")?;
    Ok(i32::from(found.is_empty()))
}

/// Asks which device of the last scan to use, then runs `adb connect`.
pub async fn connect(data_dir: &Path) -> Result<i32> {
    let path = cache_path(data_dir);
    if !path.exists() {
        find(data_dir).await?;
    }
    let list: Vec<String> = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| l.parse::<SocketAddr>().is_ok())
        .map(str::to_owned)
        .collect();
    if list.is_empty() {
        bail!("no devices found; run `adb-find` first");
    }
    for (i, d) in list.iter().enumerate() {
        println!("  {}) {d}", i + 1);
    }
    print!("Connect to [1-{}]: ", list.len());
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    let Some(target) = line
        .trim()
        .parse::<usize>()
        .ok()
        .and_then(|n| n.checked_sub(1))
        .and_then(|n| list.get(n))
    else {
        bail!("invalid choice: {}", line.trim());
    };
    let status = Command::new("adb")
        .args(["connect", target])
        .status()
        .await
        .context("cannot run `adb`; is it on PATH?")?;
    Ok(status.code().unwrap_or(1))
}
