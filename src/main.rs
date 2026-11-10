
mod proxy;

use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use librqbit::{
    AddTorrent, AddTorrentOptions, ConnectionOptions, Session, SessionOptions,
    limits::LimitsConfig,
};
use std::{
    error::Error,
    io::{self, IsTerminal},
    num::NonZeroU32,
    path::PathBuf,
    process::ExitCode,
    time::Duration,
};
use tokio::time::interval;
use url::Url;

#[derive(Debug, Parser)]
#[command(
    name = "wiretor",
    version,
    about = "A compact Rust BitTorrent downloader"
)]
struct Args {
    #[arg(value_name = "MAGNET_OR_TORRENT")]
    src: String,

    #[arg(short, long, default_value = ".")]
    out: PathBuf,

    #[arg(long, default_value_t = 100)]
    peers: usize,

    #[arg(long, value_name = "URL")]
    proxy: Option<String>,

    #[arg(long, value_name = "RATE", value_parser = parse_rate)]
    limit_download: Option<NonZeroU32>,

    #[arg(long, value_name = "RATE", value_parser = parse_rate)]
    limit_upload: Option<NonZeroU32>,

    #[arg(long, value_name = "REGEX")]
    include: Option<String>,

    #[arg(long)]
    overwrite: bool,

    #[arg(long)]
    json: bool,
}

const BALLS: usize = 16;

#[inline]
fn col(value: impl AsRef<str>, color: &str) -> String {
    let value = value.as_ref();

    if io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none() {
        format!("\x1b[{color}m{value}\x1b[0m")
    } else {
        value.to_owned()
    }
}

#[inline]
fn safe(value: &str) -> String {
    value.chars()
        .filter(|c| !c.is_control())
        .collect()
}

#[inline]
fn size(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }

    let units = ["Ki", "Mi", "Gi", "Ti", "Pi", "Ei"];
    let mut value = bytes as f64;
    let mut index = 0;

    while value >= 1024.0 && index + 1 < units.len() {
        value /= 1024.0;
        index += 1;
    }

    format!("{value:.1}{}", units[index])
}

#[inline]
fn ball(done: u64, total: u64) -> String {
    let progress = if total == 0 {
        0.0
    } else {
        (done as f64 / total as f64).clamp(0.0, 1.0)
    };

    let filled = ((progress * BALLS as f64).floor() as usize)
        .min(BALLS);

    let mut bar = String::with_capacity(BALLS * 2 + 1);

    bar.push_str(&"  ".repeat(filled));
    bar.push('C');

    for _ in filled..BALLS {
        bar.push_str(" *");
    }

    format!(
        "{bar} : {:>6.2}%  {} / {}",
        progress * 100.0,
        size(done),
        size(total),
    )
}

fn parse_rate(raw: &str) -> Result<NonZeroU32, String> {
    let raw = raw.trim();

    let split = raw
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(raw.len());

    let (number, unit) = raw.split_at(split);

    let value = number
        .parse::<u32>()
        .map_err(|_| "expected a numeric speed limit".to_owned())?;

    let multiplier = match unit.to_ascii_lowercase().as_str() {
        "" | "b" | "b/s" => 1,
        "k" | "kib" | "kib/s" => 1024,
        "m" | "mib" | "mib/s" => 1024 * 1024,
        "g" | "gib" | "gib/s" => 1024 * 1024 * 1024,
        _ => return Err("unknown speed unit; use B, K, M, or G".into()),
    };

    let bytes = value
        .checked_mul(multiplier)
        .ok_or_else(|| "speed limit is too large".to_owned())?;

    NonZeroU32::new(bytes)
        .ok_or_else(|| "speed limit must be greater than zero".to_owned())
}

fn magnet(src: &str, proxied: bool) -> Result<String, Box<dyn Error>> {
    let mut url = Url::parse(src)?;

    if url.scheme() != "magnet" {
        return Err("expected a magnet link".into());
    }

    if proxied {
        let pairs: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(key, value)| {
                !(key == "tr"
                    && value.to_ascii_lowercase().starts_with("udp://"))
            })
            .map(|(key, value)| {
                (key.into_owned(), value.into_owned())
            })
            .collect();

        url.set_query(None);

        url.query_pairs_mut().extend_pairs(
            pairs.iter().map(|(key, value)| {
                (key.as_str(), value.as_str())
            }),
        );
    }

    Ok(url.to_string())
}

#[tokio::main]
async fn main() -> ExitCode {
    match go().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!(
                "  {} {}",
                col("✗", "1;31"),
                error
            );
            ExitCode::FAILURE
        }
    }
}

async fn go() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();

    if args.peers == 0 {
        return Err("--peers must be greater than zero".into());
    }

    std::fs::create_dir_all(&args.out)?;

    let limits = LimitsConfig {
        download_bps: args.limit_download,
        upload_bps: args.limit_upload,
    };

    let proxy_url = if let Some(address) = &args.proxy {
        let (address, kind) = proxy::setup(address).await?;

        eprintln!(
            "{}",
            col(
                format!(
                    "[WIRE] {kind} proxy enabled for peer TCP connections."
                ),
                "33"
            )
        );

        Some(address)
    } else {
        None
    };

    let mut options = SessionOptions {
        peer_limit: Some(args.peers),
        ratelimits: limits,
        ..Default::default()
    };

    if let Some(address) = &proxy_url {
        options.connect = Some(ConnectionOptions {
            proxy_url: Some(address.clone()),
            enable_tcp: true,
            ..Default::default()
        });

        options.dht = None;
        options.listen = None;
        options.disable_local_service_discovery = true;
    }

    let session = Session::new_with_opts(
        args.out.clone(),
        options,
    ).await?;

    let source = if args.src.to_ascii_lowercase().starts_with("magnet:") {
        AddTorrent::from_url(
            magnet(&args.src, proxy_url.is_some())?
        )
    } else {
        AddTorrent::from_local_filename(&args.src)?
    };

    let mut add_options = AddTorrentOptions {
        peer_limit: Some(args.peers),
        ratelimits: limits,
        only_files_regex: args.include.clone(),
        overwrite: args.overwrite,
        ..Default::default()
    };

    // File selection is optional. Without a regex, all files are selected.
    if args.include.is_none() {
        add_options.only_files_regex = None;
    }

    let loading = ProgressBar::new_spinner();

    loading.set_style(
        ProgressStyle::with_template(
            "  {spinner:.cyan} {msg}"
        )?
    );

    loading.set_message("Loading torrent metadata and finding peers");
    loading.enable_steady_tick(Duration::from_millis(90));

    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    let response = session
        .add_torrent(source, Some(add_options))
        .await?;

    let handle = response
        .into_handle()
        .ok_or_else(|| io::Error::other("torrent was not added"))?;

    tokio::select! {
        result = handle.wait_until_initialized() => {
            result?;
        }

        signal = &mut shutdown => {
            signal?;
            loading.finish_and_clear();
            eprintln!("{}", col("[WIRE] Stopping session...", "33"));
            session.stop().await;
            return Ok(());
        }
    }

    loading.finish_and_clear();

    let name = safe(
        &handle.name().unwrap_or_else(|| "torrent".to_owned())
    );

    if !args.json {
        println!(
            "{}",
            col(
                format!("[WIRE] Downloading \"{name}\""),
                "30;106"
            )
        );
    }

    let initial = handle.stats();
    let total = initial.total_bytes;

    let progress = if args.json {
        ProgressBar::hidden()
    } else {
        let bar = ProgressBar::new(total);

        bar.set_style(
            ProgressStyle::with_template("{msg}")?
        );

        bar.enable_steady_tick(Duration::from_millis(120));
        bar
    };

    let download = handle.wait_until_completed();
    tokio::pin!(download);

    let mut tick = interval(Duration::from_millis(500));

    loop {
        tokio::select! {
            result = &mut download => {
                result?;

                let stats = handle.stats();
                let total = stats.total_bytes;

                if args.json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "event": "completed",
                            "name": name,
                            "downloaded": stats.progress_bytes,
                            "total": total,
                            "progress": 100.0
                        })
                    );
                } else {
                    progress.set_length(total);
                    progress.set_position(total);
                    progress.set_message(ball(total, total));
                    progress.finish();

                    println!(
                        "{}",
                        col(
                            format!(
                                "[WIRE] Download completed: {}",
                                safe(&args.out.display().to_string())
                            ),
                            "1;32"
                        )
                    );
                }

                break;
            }

            signal = &mut shutdown => {
                signal?;

                progress.finish_and_clear();

                if !args.json {
                    eprintln!(
                        "{}",
                        col("[WIRE] Stopping download...", "33")
                    );
                }

                session.stop().await;

                if !args.json {
                    eprintln!(
                        "{}",
                        col("[WIRE] Session stopped.", "1;32")
                    );
                }

                return Ok(());
            }

            _ = tick.tick() => {
                let stats = handle.stats();
                let total = stats.total_bytes;
                let downloaded = stats.progress_bytes.min(total);

                let live = stats.live
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "waiting for peers".to_owned());

                if args.json {
                    let percentage = if total == 0 {
                        0.0
                    } else {
                        downloaded as f64 / total as f64 * 100.0
                    };

                    println!(
                        "{}",
                        serde_json::json!({
                            "event": "progress",
                            "name": name,
                            "downloaded": downloaded,
                            "total": total,
                            "progress": percentage,
                            "live": live
                        })
                    );
                } else {
                    progress.set_length(total);
                    progress.set_position(downloaded);

                    progress.set_message(
                        format!("{} | {}", ball(downloaded, total), live)
                    );
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_steps() {
        let first = ball(0, 100);
        assert!(first.starts_with("C * * * *"));

        let middle = ball(50, 100);
        assert!(middle.contains("50.00%"));

        let last = ball(100, 100);
        assert!(last.contains("100.00%"));
    }

    #[test]
    fn binary_sizes() {
        assert_eq!(size(1024), "1.0Ki");
        assert_eq!(size(1024 * 1024), "1.0Mi");
    }

    #[test]
    fn rate_limits() {
        assert_eq!(parse_rate("512K").unwrap().get(), 512 * 1024);
        assert_eq!(parse_rate("5M").unwrap().get(), 5 * 1024 * 1024);
        assert_eq!(parse_rate("1024").unwrap().get(), 1024);

        assert!(parse_rate("0").is_err());
        assert!(parse_rate("invalid").is_err());
        assert!(parse_rate("5T").is_err());
    }

    #[test]
    fn tracker_filter() {
        let src = "magnet:?xt=urn:btih:abc&tr=udp%3A%2F%2Ftracker.test%3A80&tr=https%3A%2F%2Ftracker.test%2Fannounce";

        let filtered = magnet(src, true).unwrap();
        assert!(!filtered.contains("udp%3A"));
        assert!(filtered.contains("https%3A"));

        let original = magnet(src, false).unwrap();
        assert!(original.contains("udp%3A"));
    }
}

