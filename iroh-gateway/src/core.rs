use axum::Router;
use futures::ready;
use futures::stream::StreamExt;
use hyper::server::accept::Accept;
use iroh_rpc_client::Client as RpcClient;
use iroh_rpc_types::gateway::GatewayServerAddr;
use tokio_stream::wrappers::TcpListenerStream;

use std::{
    collections::HashMap,
    net::{SocketAddr, TcpListener},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::sync::RwLock;

use crate::{
    bad_bits::BadBits,
    client::Client,
    handlers::{get_app_routes, StateConfig},
    rpc,
    rpc::Gateway,
    templates,
};

#[derive(Debug, Clone)]
pub struct Core {
    state: Arc<State>,
}

#[derive(Debug, Clone)]
pub struct State {
    pub config: Arc<dyn StateConfig>,
    pub client: Client,
    pub handlebars: HashMap<String, String>,
    pub bad_bits: Arc<Option<RwLock<BadBits>>>,
}

impl Core {
    pub async fn new(
        config: Arc<dyn StateConfig>,
        rpc_addr: Option<GatewayServerAddr>,
        bad_bits: Arc<Option<RwLock<BadBits>>>,
    ) -> anyhow::Result<Self> {
        if let Some(raddr) = rpc_addr {
            tokio::spawn(async move {
            // TODO: handle error
            rpc::new(raddr, Gateway::default()).await
        });
        }
        
        let rpc_client = RpcClient::new(config.rpc_client().clone()).await?;
        let mut templates = HashMap::new();
        templates.insert("dir_list".to_string(), templates::DIR_LIST.to_string());
        templates.insert("not_found".to_string(), templates::NOT_FOUND.to_string());
        let client = Client::new(&rpc_client);

        Ok(Self {
            state: Arc::new(State {
                config,
                client,
                handlebars: templates,
                bad_bits,
            }),
        })
    }

    pub async fn new_with_state(
        rpc_addr: GatewayServerAddr,
        state: Arc<State>,
    ) -> anyhow::Result<Self> {
        tokio::spawn(async move {
            // TODO: handle error
            rpc::new(rpc_addr, Gateway::default()).await
        });
        Ok(Self { state })
    }

    pub async fn make_state(
        config: Arc<dyn StateConfig>,
        bad_bits: Arc<Option<RwLock<BadBits>>>,
    ) -> anyhow::Result<Arc<State>> {
        let rpc_client = RpcClient::new(config.rpc_client().clone()).await?;
        let mut templates = HashMap::new();
        templates.insert("dir_list".to_string(), templates::DIR_LIST.to_string());
        templates.insert("not_found".to_string(), templates::NOT_FOUND.to_string());
        let client = Client::new(&rpc_client);
        Ok(Arc::new(State {
            config,
            client,
            handlebars: templates,
            bad_bits,
        }))
    }

    pub fn server(
        self,
    ) -> axum::Server<hyper::server::conn::AddrIncoming, axum::routing::IntoMakeService<Router>>
    {
        let app = get_app_routes(&self.state);

        // todo(arqu): make configurable
        let addr = format!("0.0.0.0:{}", self.state.config.port());

        // axum::Server::bind(&addr.parse().unwrap())
        //     .http1_preserve_header_case(true)
        //     .http1_title_case_headers(true)
        //     .serve(app.into_make_service())

        let addr: std::net::SocketAddr = addr.parse().unwrap();
        let sock = socket2::Socket::new(
            match addr {
                SocketAddr::V4(_) => socket2::Domain::IPV4,
                SocketAddr::V6(_) => socket2::Domain::IPV6,
            },
            socket2::Type::STREAM,
            None,
        )
        .unwrap();

        sock.set_reuse_address(true).unwrap();
        sock.set_reuse_port(true).unwrap();
        sock.set_nonblocking(true).unwrap();
        sock.set_nodelay(true).unwrap();
        sock.bind(&addr.into()).unwrap();
        sock.listen(8192 * 2).unwrap();

        // let incoming =
        //     TcpListenerStream::new(TcpListener::from_std(sock.into()).unwrap());

        let incoming: TcpListener = sock.into();

        axum::Server::from_tcp(incoming)
            .unwrap()
            .http1_preserve_header_case(true)
            .http1_title_case_headers(true)
            .serve(app.into_make_service())
    }

    pub async fn serve_internal(c: Core) {
        let server = c.server();
        println!("HTTP endpoint listening on {}", server.local_addr());
        server.await.unwrap();
    }

    pub fn multi_server(
        self,
    )
    {
        let mut handlers = Vec::new();
        for i in 0..num_cpus::get() {
            let c = self.clone();
            let h = std::thread::spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(Core::serve_internal(c));
            });
            handlers.push(h);
        }

        // for h in handlers {
        //     h.join().unwrap();
        // }
    }
}

// pub struct ServerAcceptSharedPort {
//     pub tcpls: TcpListener,
// }

// impl Accept for ServerAcceptSharedPort {
//     type Conn = TcpStream;
//     type Error = Box<dyn std::error::Error + Send + Sync>;

//     fn poll_accept(
//         self: Pin<&mut Self>,
//         cx: &mut Context<'_>,
//     ) -> Poll<Option<Result<Self::Conn, Self::Error>>> {
//         // self.tcpls.poll_accept(cx)
//         // Poll::Ready(Ok((stream, _))) => Poll::Ready(Some(Ok(stream))),
//         let (stream, _addr) = ready!(self.tcpls.poll_accept(cx))?;
//         Poll::Ready(Some(Ok(stream)))
//     }
// }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use iroh_rpc_client::Config as RpcClientConfig;

    #[tokio::test]
    async fn gateway_health() {
        let mut config = Config::new(
            false,
            false,
            false,
            0,
            RpcClientConfig {
                gateway_addr: None,
                p2p_addr: None,
                store_addr: None,
            },
        );
        config.set_default_headers();

        // let rpc_addr = "grpc://0.0.0.0:0".parse().unwrap();
        let handler = Core::new(Arc::new(config), None, Arc::new(None))
            .await
            .unwrap();
        let server = handler.server();
        let addr = server.local_addr();
        let core_task = tokio::spawn(async move {
            server.await.unwrap();
        });

        let uri = hyper::Uri::builder()
            .scheme("http")
            .authority(format!("localhost:{}", addr.port()))
            .path_and_query("/health")
            .build()
            .unwrap();
        let client = hyper::Client::new();
        let res = client.get(uri).await.unwrap();

        assert_eq!(http::StatusCode::OK, res.status());
        let body = hyper::body::to_bytes(res.into_body()).await.unwrap();
        assert_eq!(b"OK", &body[..]);
        core_task.abort();
        core_task.await.unwrap_err();
    }
}
