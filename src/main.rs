
mod proxy;

use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use librqbit::{AddTorrent, AddTorrentOptions, ConnectionOptions, Session, SessionOptions};
use std::{
    error::Error,
    io::{self, IsTerminal},
    path::PathBuf,
    process::ExitCode,
    time::Duration,
};
use url::Url;

#[derive(Debug, Parser)]
#[command(name = "wiretor", version, about = "A compact Rust BitTorrent downloader")]
struct Args {
    #[arg(value_name = "magnet or torrent")]
    src: String,
    #[arg(short, long, default_value = ".")]
    out: PathBuf,
    #[arg(long, default_value_t = 100)]
    peers: usize,
    #[arg(long, value_name = "url")]
    proxy: Option<String>,
}

#[inline]
fn col(s: impl AsRef<str>, c: &str) -> String {
    let s = s.as_ref();
    if io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none() {
        format!("\x1b[{c}m{s}\x1b[0m")
    } else {
        s.to_owned()
    }
}

#[inline]
fn safe(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

const BALLS: usize = 16;

#[inline]
fn size(n: u64) -> String {
    if n < 1024 {
        return format!("{n} B");
    }

    let units = ["Ki", "Mi", "Gi", "Ti", "Pi", "Ei"];
    let mut n = n as f64;
    let mut i = 0;

    while n >= 1024.0 && i + 1 < units.len() {
        n /= 1024.0;
        i += 1;
    }

    format!("{n:.1}{}", units[i])
}

#[inline]
fn ball(done: u64, total: u64) -> String {
    let f = if total == 0 {
        0.0
    } else {
        (done as f64 / total as f64).clamp(0.0, 1.0)
    };

    let n = ((f * BALLS as f64).floor() as usize).min(BALLS);
    let mut bar = String::with_capacity(BALLS * 2 + 1);

    bar.push_str(&"  ".repeat(n));
    bar.push('C');

    for _ in n..BALLS {
        bar.push_str(" *");
    }

    format!(
        "{bar} : {:>6.2}%  {} / {} / {total}",
        f * 100.0,
        size(done),
        size(total),
    )
}

fn magnet(src: &str, proxied: bool) -> Result<String, Box<dyn Error>> {
    let mut url = Url::parse(src)?;

    if url.scheme() != "magnet" {
        return Err("expected a magnet link".into());
    }

    if proxied {
        let pairs: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(k, v)| {
                !(k == "tr" && v.to_ascii_lowercase().starts_with("udp://"))
            })
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();

        url.set_query(None);
        url.query_pairs_mut()
            .extend_pairs(pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    }

    Ok(url.to_string())
}

#[tokio::main]
async fn main() -> ExitCode {
    match go().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("  {} {}", col("✗", "1;31"), e);
            ExitCode::FAILURE
        }
    }
}

async fn go() -> Result<(), Box<dyn Error>> {
    let a = Args::parse();

    let _a = 0u8;
    let _b = false;

    if a.peers == 0 {
        return Err("--peers must be greater than zero".into());
    }

    let p = if let Some(v) = &a.proxy {
        let (c, k) = proxy::setup(v).await?;
        eprintln!(
            "{}",
            col(
                format!(
                    "[WIRE] {k} proxy enabled for peer TCP connections,tracker traffic may connect directly."
                ),
                "33"
            )
        );
        Some(c)
    } else {
        None
    };

    std::fs::create_dir_all(&a.out)?;
    let proxied = p.is_some();

    let mut so = SessionOptions {
        peer_limit: Some(a.peers),
        ..Default::default()
    };

    if let Some(v) = &p {
        so.connect = Some(ConnectionOptions {
            proxy_url: Some(v.clone()),
            enable_tcp: true,
            ..Default::default()
        });
        so.dht = None;
        so.listen = None;
        so.disable_local_service_discovery = true;
    }

    let s = Session::new_with_opts(a.out.clone(), so).await?;

    let src = if a.src.to_ascii_lowercase().starts_with("magnet:") {
        AddTorrent::from_url(magnet(&a.src, proxied)?)
    } else {
        AddTorrent::from_local_filename(&a.src)?
    };

    let ui = ProgressBar::new_spinner();
    ui.set_style(ProgressStyle::with_template("  {spinner:.cyan} {msg}")?);
    ui.set_message("Loading torrent metadata and finding peers");
    ui.enable_steady_tick(Duration::from_millis(90));

    let r = s
        .add_torrent(
            src,
            Some(AddTorrentOptions {
                peer_limit: Some(a.peers),
                ..Default::default()
            }),
        )
        .await?;

    let h = r
        .into_handle()
        .ok_or_else(|| io::Error::other("torrent was not added"))?;

    h.wait_until_initialized().await?;
    ui.finish_and_clear();

    let name = safe(&h.name().unwrap_or_else(|| "torrent".to_owned()));
    println!("{}", col(format!("[WIRE] Downloading \"{name}\""), "30;106"));

    let st = h.stats();
    let n = st.total_bytes;

    let pb = ProgressBar::new(n);
    pb.set_style(ProgressStyle::with_template("{msg}")?);
    pb.set_message(ball(st.progress_bytes, n));
    pb.set_position(st.progress_bytes.min(n));
    pb.enable_steady_tick(Duration::from_millis(120));

    let dl = h.wait_until_completed();
    tokio::pin!(dl);

    let mut tick = tokio::time::interval(Duration::from_millis(500));

    loop {
        tokio::select! {
            r = &mut dl => {
                r?;

                let st = h.stats();
                let n = st.total_bytes;

                pb.set_length(n);
                pb.set_position(n);
                pb.set_message(ball(n, n));
                pb.finish();

                break;
            }

            _ = tick.tick() => {
                let st = h.stats();
                let n = st.total_bytes;

                pb.set_length(n);
                pb.set_position(st.progress_bytes.min(n));
                pb.set_message(ball(st.progress_bytes, n));
            }
        }
    }

    println!(
        "{}",
        col(
            format!("[OK] Downloaded to {}!", safe(&a.out.display().to_string())),
            "1;32"
        )
    );

    Ok(())
}

//ignore
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ballsateps() {
        let a = ball(0, 100);
        assert!(a.starts_with("C * * * * * * * * * * * * * * * * :"));

        let b = ball(50, 100);
        assert!(b.starts_with("                C * * * * * * * * :"));

        let c = ball(100, 100);
        assert!(c.starts_with("                                C :"));
        assert!(c.contains("100.00%"));
    }

    #[test]
    fn binarysizes() {
        assert_eq!(size(1024), "1.0Ki");
        assert_eq!(size(1024 * 1024), "1.0Mi");
    }

    #[test]
    fn trackerfilter() {
        let src = "magnet:?xt=urn:btih:abc&tr=udp%3A%2F%2Ftracker.test%3A80&tr=https%3A%2F%2Ftracker.test%2Fannounce";

        let a = magnet(src, true).unwrap();
        assert!(!a.contains("udp%3A"));
        assert!(a.contains("https%3A"));

        let b = magnet(src, false).unwrap();
        assert!(b.contains("udp%3A"));
    }
}

