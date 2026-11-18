//! docket CLI binary.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    docket_cli::run_cli().await
}
