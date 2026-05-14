//! Development server for the Cairn desktop GUI alpha.

use std::net::SocketAddr;

use cairn_desktop::{fixture::DesktopFixture, repository::DesktopRepository, server::router};

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("warn,cairn_desktop=info")
        .init();

    let fixture = DesktopFixture::load_default()?;
    let app = router(DesktopRepository::from_fixture(fixture));
    let addr = SocketAddr::from(([127, 0, 0, 1], 0));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let actual = listener.local_addr()?;
    println!("cairn-desktop listening on http://{actual}");
    axum::serve(listener, app).await?;
    Ok(())
}
