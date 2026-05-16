// rusttorrentd — Phase 0 stub
fn main() {
    tracing_subscriber::fmt().json().init();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "rusttorrentd starting");
}
