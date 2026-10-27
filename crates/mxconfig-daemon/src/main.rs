//! mxconfigd: serves installed and registered namespaces on the session or system bus.

use anyhow::{Context, Result};
use clap::Parser;
use mxconfig::Bus;
use mxconfig::host::Roots;
use mxconfig::host::protocol::{BUS_NAME, OBJECT_PATH};
use mxconfig_daemon::Service;
use zbus::connection::Builder;

#[derive(Parser)]
#[command(name = "mxconfigd", about = "The mxconfig daemon")]
struct Cli {
    /// Serve privileged namespaces on the system bus, behind polkit.
    #[arg(long)]
    privileged: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let (builder, bus) = if cli.privileged {
        (Builder::system()?, Bus::System)
    } else {
        (Builder::session()?, Bus::Session)
    };
    let service = Service::new(builder.build().await?, Roots::find(), bus);
    service.publish().await.context("publishing the service")?;
    eprintln!("mxconfigd serving {OBJECT_PATH} on `{BUS_NAME}`");

    std::future::pending().await
}
