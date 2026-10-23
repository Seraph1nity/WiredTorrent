# WiredTorrent

A compact Rust built bittorrent downloader. Made for low-end computers and almost no resource use during download,fast and compact.

## Build

Install Rust, then install WiredTorrent as the `wiretor` command:

```sh
cargo install --path .
```

To build without installing it globally, run `cargo build --release`; the executable is `target/release/wiretor` on Linux/macOS (`target/release/wiretor.exe` on Windows).

## Usage

```sh
# save in the current working directory (default)
wiretor 'magnet:?xt=urn:btih:...'

# local .torrent file
wiretor ./cooltorrentfromlegalsite.torrent

# choose output directory and limit peer connections
wiretor './example.torrent' --out ./downloads --peers 80

# SOCKS5 proxy
wiretor 'magnet:?xt=urn:btih:...' --proxy socks5://127.0.0.1:9050

# SOCKS5 with auth
wiretor 'magnet:?xt=urn:btih:...' --proxy 'socks5://user:password@127.0.0.1:1080'

# HTTP CONNECT proxy
wiretor 'magnet:?xt=urn:btih:...' --proxy http://127.0.0.1:8080

# HTTPS proxy
wiretor 'magnet:?xt=urn:btih:...' --proxy https://proxy.example:8443

# HTTPS proxy with auth
wiretor 'magnet:?xt=urn:btih:...' --proxy 'https://user:password@proxy.example:8443'
```

`--out` defaults to `.` (the process current working directory); `--peers` defaults to `100`. The live progress line uses 16 stars: `C` moves right and eats one star for each 1/16 of progress,then finishes with all stars consumed at 100%.
## Notes

- This program is intended for downloading content you are authorized to access.
- Torrent data is written to disk by `librqbit` during downloading; it is not buffered entirely in RAM.
---

## License

WiredTorrent is licensed under the [GPL-3.0 License](LICENSE).

See the [LICENSE](LICENSE) file for the full license terms.
