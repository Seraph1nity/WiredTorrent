
use base64::{engine::general_purpose::STANDARD, Engine as _};
use percent_encoding::percent_decode_str;
use std::{
    error::Error,
    net::{Ipv4Addr, Ipv6Addr},
    sync::Arc,
};
use tokio::{
    io::{self, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tokio_rustls::{
    rustls::{pki_types::ServerName, ClientConfig, RootCertStore},
    TlsConnector,
};
use url::Url;

const MAX: usize = 16 * 1024;

type Io = Box<dyn Stream>;
trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

struct Config {
    url: Url,
    tls: Option<TlsConnector>,
}

pub async fn setup(raw: &str) -> Result<(String, &'static str), Box<dyn Error>> {
    let url = Url::parse(raw)?;
    let host = url.host_str().ok_or("proxy URL must include a hostname")?;

    let _a = 0u8;
    let _b = false;

    match url.scheme() {
        "socks5" => {
            if url.port().is_none() {
                return Err("SOCKS5 proxy needs an explicit port: socks5://host:port".into());
            }

            if host.is_empty()
                || url.path() != "/"
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err("invalid SOCKS5 proxy URL".into());
            }

            Ok((raw.to_owned(), "SOCKS5"))
        }
        "http" | "https" => {
            if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
                return Err("proxy URL must contain only scheme, credentials, host and optional port".into());
            }

            if url.port_or_known_default().is_none() {
                return Err("proxy URL has no port".into());
            }

            let label = if url.scheme() == "https" {
                "HTTPS CONNECT"
            } else {
                "HTTP CONNECT"
            };

            let socks = bridge(url).await?;
            Ok((socks, label))
        }
        _ => Err("--proxy supports socks5://, http://, or https:// proxy URLs".into()),
    }
}

async fn bridge(url: Url) -> Result<String, Box<dyn Error>> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let addr = listener.local_addr()?;

    let tls = if url.scheme() == "https" {
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

        let cfg = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();

        Some(TlsConnector::from(Arc::new(cfg)))
    } else {
        None
    };

    let cfg = Arc::new(Config { url, tls });

    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((client, _)) => {
                    let cfg = Arc::clone(&cfg);

                    tokio::spawn(async move {
                        let _ = clientrun(client, cfg).await;
                    });
                }
                Err(e) => {
                    eprintln!("[WIRE] Proxy bridge stopped: {e}");
                    break;
                }
            }
        }
    });

    Ok(format!("socks5://{addr}"))
}

async fn clientrun(mut client: TcpStream, cfg: Arc<Config>) -> io::Result<()> {
    let mut head = [0u8; 2];
    client.read_exact(&mut head).await?;

    if head[0] != 5 || head[1] == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid SOCKS5 greeting"));
    }

    let mut methods = vec![0u8; head[1] as usize];
    client.read_exact(&mut methods).await?;

    if !methods.contains(&0) {
        client.write_all(&[5, 0xff]).await?;
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "SOCKS5 no-auth method not offered",
        ));
    }

    client.write_all(&[5, 0]).await?;

    let (host, port) = match target(&mut client).await {
        Ok(v) => v,
        Err(e) => {
            let _ = reply(&mut client, 1).await;
            return Err(e);
        }
    };

    let mut upstream = match connect(&cfg, &host, port).await {
        Ok(v) => v,
        Err(e) => {
            let _ = reply(&mut client, 5).await;
            return Err(e);
        }
    };

    reply(&mut client, 0).await?;
    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;

    Ok(())
}

async fn target(client: &mut TcpStream) -> io::Result<(String, u16)> {
    let mut head = [0u8; 4];
    client.read_exact(&mut head).await?;

    if head[0] != 5 || head[2] != 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid SOCKS5 request"));
    }

    if head[1] != 1 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "SOCKS5 supports TCP CONNECT only",
        ));
    }

    let host = match head[3] {
        1 => {
            let mut bytes = [0u8; 4];
            client.read_exact(&mut bytes).await?;
            Ipv4Addr::from(bytes).to_string()
        }
        3 => {
            let mut len = [0u8; 1];
            client.read_exact(&mut len).await?;

            if len[0] == 0 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "empty SOCKS5 hostname"));
            }

            let mut bytes = vec![0u8; len[0] as usize];
            client.read_exact(&mut bytes).await?;

            String::from_utf8(bytes)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid SOCKS5 hostname"))?
        }
        4 => {
            let mut bytes = [0u8; 16];
            client.read_exact(&mut bytes).await?;
            Ipv6Addr::from(bytes).to_string()
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported SOCKS5 address type",
            ));
        }
    };

    if host.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid SOCKS5 destination hostname",
        ));
    }

    let mut port = [0u8; 2];
    client.read_exact(&mut port).await?;

    Ok((host, u16::from_be_bytes(port)))
}

async fn reply(client: &mut TcpStream, status: u8) -> io::Result<()> {
    client
        .write_all(&[5, status, 0, 1, 0, 0, 0, 0, 0, 0])
        .await
}

async fn connect(cfg: &Config, host: &str, port: u16) -> io::Result<Io> {
    let ph = cfg
        .url
        .host_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "proxy hostname missing"))?
        .to_owned();

    let pp = cfg
        .url
        .port_or_known_default()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "proxy port missing"))?;

    let tcp = TcpStream::connect((ph.as_str(), pp)).await?;

    let mut upstream: Io = if let Some(tls) = &cfg.tls {
        let name = ServerName::try_from(ph.clone()).map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidInput, format!("invalid HTTPS proxy hostname: {e}"))
        })?;

        Box::new(
            tls.connect(name, tcp).await.map_err(|e| {
                io::Error::new(io::ErrorKind::ConnectionAborted, format!("TLS to HTTPS proxy failed: {e}"))
            })?,
        )
    } else {
        Box::new(tcp)
    };

    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };

    let addr = format!("{host}:{port}");
    let auth = auth(&cfg.url);

    let req = format!(
        "CONNECT {addr} HTTP/1.1\r\nHost: {addr}\r\nProxy-Connection: Keep-Alive\r\n{auth}\r\n"
    );

    upstream.write_all(req.as_bytes()).await?;

    let res = headers(&mut upstream).await?;
    let line = String::from_utf8_lossy(
        res.split(|b| *b == b'\n').next().unwrap_or_default(),
    );

    let status = line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok());

    if status != Some(200) {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            format!("HTTP proxy rejected CONNECT ({})", line.trim()),
        ));
    }

    Ok(upstream)
}

fn auth(url: &Url) -> String {
    if url.username().is_empty() && url.password().is_none() {
        return String::new();
    }

    let user = percent_decode_str(url.username()).decode_utf8_lossy();

    let pass = url
        .password()
        .map(|s| percent_decode_str(s).decode_utf8_lossy().into_owned())
        .unwrap_or_default();

    let creds = format!("{user}:{pass}");
    format!(
        "Proxy-Authorization: Basic {}\r\n",
        STANDARD.encode(creds.as_bytes())
    )
}

async fn headers(stream: &mut Io) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(256);

    while bytes.len() < MAX {
        let mut byte = [0u8; 1];

        if stream.read(&mut byte).await? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "proxy closed before CONNECT response",
            ));
        }

        bytes.push(byte[0]);

        if bytes.ends_with(b"\r\n\r\n") {
            return Ok(bytes);
        }
    }

    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "HTTP proxy response headers too large",
    ))
}

