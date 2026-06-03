use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    marsala::run().await
}
