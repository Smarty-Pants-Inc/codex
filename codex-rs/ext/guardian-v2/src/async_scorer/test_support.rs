use anyhow::Result;
use core_test_support::responses;
use tokio::net::TcpListener;
use tokio::net::TcpStream;

#[derive(Clone, Copy)]
pub(super) enum ProxyPrewarmLimit {
    AllConnections,
    StopAfter { ready_connections: usize },
}

pub(super) async fn proxy_websocket_servers(
    servers: &[&responses::WebSocketTestServer],
) -> Result<String> {
    proxy_websocket_servers_with_prewarm_limit(servers, ProxyPrewarmLimit::AllConnections).await
}

pub(super) async fn proxy_websocket_servers_with_prewarm_limit(
    servers: &[&responses::WebSocketTestServer],
    prewarm_limit: ProxyPrewarmLimit,
) -> Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let targets = servers
        .iter()
        .map(|server| server.uri().trim_start_matches("ws://").to_owned())
        .collect::<Vec<_>>();
    tokio::spawn(async move {
        for (index, target) in targets.into_iter().enumerate() {
            if let ProxyPrewarmLimit::StopAfter { ready_connections } = prewarm_limit
                && index == ready_connections
            {
                let Ok((connection, _)) = listener.accept().await else {
                    return;
                };
                drop(connection);
            }
            let Ok((mut incoming, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let Ok(mut outgoing) = TcpStream::connect(target).await else {
                    return;
                };
                let _ = tokio::io::copy_bidirectional(&mut incoming, &mut outgoing).await;
            });
        }
    });
    Ok(format!("http://{address}/v1"))
}
