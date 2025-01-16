use mod_libp2p::network::EventLoop;
use tracing::error;
mod mod_libp2p;

#[tokio::main]
async fn main() {
    dotenv::dotenv().ok();
    std::env::set_var("RUST_LOG", "info");

    tracing_subscriber::fmt()
        .with_ansi(false)
        .event_format(
            tracing_subscriber::fmt::format()
                .with_file(true)
                .with_line_number(true),
        )
        .init();

    match EventLoop::new().await {
        Ok(mut event_loop) => {
            event_loop.run().await;
        }
        Err(err) => error!("start libp2p swarm failed, the err is {:?}", err),
    }
}
