#![feature(let_chains)]

mod client;
mod queue;
mod server;

use std::sync::Arc;

use tokio::sync::RwLock;

use crate::{client::Client, queue::Queue, server::Server};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();

    let queue = Arc::new(RwLock::new(Queue::default()));
    let client = Client::new(queue.clone()).await;
    let server = Server::new(
        queue.clone(),
        client.get_packets(),
        client.c2s.clone(),
        client.s2c.clone(),
    );

    let is_standalone = client.is_standalone.clone();
    let server_task = tokio::spawn(async move {
        _ = server.listen(is_standalone).await;
    });
    let client_task = tokio::spawn(async move {
        client.connect().await.unwrap();
    });

    _ = tokio::try_join!(client_task, server_task);

    Ok(())
}
