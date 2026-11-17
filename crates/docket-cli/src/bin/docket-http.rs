//! docket HTTP server binary.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    docket_cli::run_http().await
}
