#![feature(div_duration)]
#![feature(let_chains)]

mod client;
mod queue;
mod server;

use crate::client::Client;
use crate::server::Server;
use std::sync::Arc;

use crate::queue::Queue;
use tokio::sync::RwLock;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let queue = Arc::new(RwLock::new(Queue::new()));
    let mut client = Client::new(queue.clone()).await;
    let mut server = Server::new(queue.clone(), client.get_packets(), client.c2s.clone(), client.s2c.clone()).await;

    let is_standalone = client.is_standalone.clone();
    let server_task = tokio::spawn(async move {
        _ = server.listen(is_standalone).await;
    });
    let client_task = tokio::spawn(async move {
        _ = client.connect().await;
    });

    _ = tokio::try_join!(client_task, server_task);

    Ok(())
}
