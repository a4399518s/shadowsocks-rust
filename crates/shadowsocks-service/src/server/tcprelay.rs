//! Shadowsocks TCP server

use std::{
    future::Future,
    io::{self, ErrorKind},
    net::SocketAddr,
    sync::Arc,
    time::Duration,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use log::{debug, error, info, trace, warn};
use shadowsocks::{
    crypto::CipherKind,
    net::{AcceptOpts, TcpStream as OutboundTcpStream},
    relay::tcprelay::{utils::copy_encrypted_bidirectional, ProxyServerStream},
    ProxyListener, ServerConfig,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream as TokioTcpStream,
    time,
};
use shadowsocks::config::RunInfo;
use shadowsocks::util::{dump_info, dump_pid};
use crate::net::{utils::ignore_until_end, MonProxyStream};
use super::context::ServiceContext;

/// TCP server instance
pub struct TcpServer {
    context: Arc<ServiceContext>,
    pub svr_cfg: ServerConfig,
    listener: ProxyListener,
    pub run_info: Arc<RunInfo>,
}

impl TcpServer {
    pub(crate) async fn new(
        context: Arc<ServiceContext>,
        svr_cfg: ServerConfig,
        accept_opts: AcceptOpts,
    ) -> io::Result<TcpServer> {
        let listener = ProxyListener::bind_with_opts(context.context(), &svr_cfg, accept_opts).await?;
        Ok(TcpServer {
            context,
            listener,
            run_info:svr_cfg.run_info.clone(),
            svr_cfg,

        })
    }

    /// Server's configuration
    pub fn server_config(&self) -> &ServerConfig {
        &self.svr_cfg
    }

    /// Server's listen address
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Start server's accept loop
    pub async fn run(self) -> io::Result<()> {
        info!(
            "shadowsocks tcp server listening on {}, inbound address {}",
            self.listener.local_addr().expect("listener.local_addr"),
            self.svr_cfg.addr()
        );
        dump_pid(self.svr_cfg.dir.clone().unwrap(),self.svr_cfg.id.clone().unwrap(),self.svr_cfg.pid);

        let dir = self.svr_cfg.dir.clone().unwrap();
        let id = self.svr_cfg.id.clone().unwrap();
        let pid = self.svr_cfg.pid;
        let run_info = self.run_info.clone();
        tokio::spawn(async move {
            loop {
                time::sleep(time::Duration::from_secs(1)).await;
                dump_info(&dir,&id,pid,run_info.use_up_sum.load(Ordering::Relaxed),run_info.use_down_sum.load(Ordering::Relaxed));
            }
        });
        loop {
            let current_time = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();
            if current_time > self.svr_cfg.expire {
                panic!("current_time > expire {current_time} > {}",self.svr_cfg.expire)
            }
            if self.run_info.use_down_sum.load(Ordering::Relaxed) > self.svr_cfg.max_down {
                let dir = self.svr_cfg.dir.clone().unwrap();
                let id = self.svr_cfg.id.clone().unwrap();
                let pid = self.svr_cfg.pid;
                dump_info(&dir,&id,pid,self.run_info.use_up_sum.load(Ordering::Relaxed),self.run_info.use_down_sum.load(Ordering::Relaxed));
                panic!("self.use_down_sum > self.max_down {} > {}",self.run_info.use_down_sum.load(Ordering::Relaxed),self.svr_cfg.max_down)
            }

            let flow_stat = self.context.flow_stat();

            let (local_stream, peer_addr) = match self
                .listener
                .accept_map(|s| MonProxyStream::from_stream(s, flow_stat))
                .await
            {
                Ok(s) => s,
                Err(err) => {
                    error!("tcp server accept failed with error: {}", err);
                    time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            };

            if self.context.check_client_blocked(&peer_addr) {
                warn!("access denied from {} by ACL rules", peer_addr);
                continue;
            }

            let client = TcpServerClient {
                context: self.context.clone(),
                method: self.svr_cfg.method(),
                peer_addr,
                stream: local_stream,
                timeout: self.svr_cfg.timeout(),
                run_info: self.run_info.clone(),
            };

            tokio::spawn(async move {
                if let Err(err) = client.serve().await {
                    debug!("tcp server stream aborted with error: {}", err);
                }
            });
        }
    }
}

#[inline]
async fn timeout_fut<F, R>(duration: Option<Duration>, f: F) -> io::Result<R>
where
    F: Future<Output = io::Result<R>>,
{
    match duration {
        None => f.await,
        Some(d) => match time::timeout(d, f).await {
            Ok(o) => o,
            Err(..) => Err(ErrorKind::TimedOut.into()),
        },
    }
}

struct TcpServerClient {
    context: Arc<ServiceContext>,
    method: CipherKind,
    peer_addr: SocketAddr,
    stream: ProxyServerStream<MonProxyStream<TokioTcpStream>>,
    timeout: Option<Duration>,
    pub run_info: Arc<RunInfo>,
}

impl TcpServerClient {
    async fn serve(mut self) -> io::Result<()> {
        // let target_addr = match Address::read_from(&mut self.stream).await {
        let target_addr = match timeout_fut(self.timeout, self.stream.handshake()).await {
            Ok(a) => a,
            // Err(Socks5Error::IoError(ref err)) if err.kind() == ErrorKind::UnexpectedEof => {
            //     debug!(
            //         "handshake failed, received EOF before a complete target Address, peer: {}",
            //         self.peer_addr
            //     );
            //     return Ok(());
            // }
            Err(err) if err.kind() == ErrorKind::UnexpectedEof => {
                debug!(
                    "tcp handshake failed, received EOF before a complete target Address, peer: {}",
                    self.peer_addr
                );
                return Ok(());
            }
            Err(err) if err.kind() == ErrorKind::TimedOut => {
                debug!(
                    "tcp handshake failed, timeout before a complete target Address, peer: {}",
                    self.peer_addr
                );
                return Ok(());
            }
            Err(err) => {
                // https://github.com/shadowsocks/shadowsocks-rust/issues/292
                //
                // Keep connection open. Except AEAD-2022
                warn!("tcp handshake failed. peer: {}, {}", self.peer_addr, err);

                #[cfg(feature = "aead-cipher-2022")]
                if self.method.is_aead_2022() {
                    // Set SO_LINGER(0) for misbehave clients, which will eventually receive RST. (ECONNRESET)
                    // This will also prevent the socket entering TIME_WAIT state.

                    let stream = self.stream.into_inner().into_inner();
                    let _ = stream.set_linger(Some(Duration::ZERO));

                    return Ok(());
                }

                debug!("tcp silent-drop peer: {}", self.peer_addr);

                // Unwrap and get the plain stream.
                // Otherwise it will keep reporting decryption error before reaching EOF.
                //
                // Note: This will drop all data in the decryption buffer, which is no going back.
                let mut stream = self.stream.into_inner();

                let res = ignore_until_end(&mut stream).await;

                trace!(
                    "tcp silent-drop peer: {} is now closing with result {:?}",
                    self.peer_addr,
                    res
                );

                return Ok(());
            }
        };

        trace!(
            "accepted tcp client connection {}, establishing tunnel to {}",
            self.peer_addr,
            target_addr
        );

        if self.context.check_outbound_blocked(&target_addr).await {
            error!(
                "tcp client {} outbound {} blocked by ACL rules",
                self.peer_addr, target_addr
            );
            return Ok(());
        }

        let mut remote_stream = match timeout_fut(
            self.timeout,
            OutboundTcpStream::connect_remote_with_opts(
                self.context.context_ref(),
                &target_addr,
                self.context.connect_opts_ref(),
            ),
        )
        .await
        {
            Ok(s) => s,
            Err(err) => {
                error!(
                    "tcp tunnel {} -> {} connect failed, error: {}",
                    self.peer_addr, target_addr, err
                );
                return Err(err);
            }
        };

        // https://github.com/shadowsocks/shadowsocks-rust/issues/232
        //
        // Protocols like FTP, clients will wait for servers to send Welcome Message without sending anything.
        //
        // Wait at most 500ms, and then sends handshake packet to remote servers.
        if self.context.connect_opts_ref().tcp.fastopen {
            let mut buffer = [0u8; 8192];
            match time::timeout(Duration::from_millis(500), self.stream.read(&mut buffer)).await {
                Ok(Ok(0)) => {
                    // EOF. Just terminate right here.
                    return Ok(());
                }
                Ok(Ok(n)) => {
                    // Send the first packet.
                    timeout_fut(self.timeout, remote_stream.write_all(&buffer[..n])).await?;
                }
                Ok(Err(err)) => return Err(err),
                Err(..) => {
                    // Timeout. Send handshake to server.
                    timeout_fut(self.timeout, remote_stream.write(&[])).await?;

                    trace!(
                        "tcp tunnel {} -> {} sent TFO connect without data",
                        self.peer_addr,
                        target_addr
                    );
                }
            }
        }

        debug!(
            "established tcp tunnel {} <-> {} with {:?}",
            self.peer_addr,
            target_addr,
            self.context.connect_opts_ref()
        );

        match copy_encrypted_bidirectional(self.method, &mut self.stream, &mut remote_stream).await {
            Ok((rn, wn)) => {
                self.run_info.use_up_sum.fetch_add(rn,Ordering::Relaxed);
                self.run_info.use_down_sum.fetch_add(wn,Ordering::Relaxed);
                trace!(
                    "tcp tunnel {} <-> {} closed, L2R {} bytes, R2L {} bytes",
                    self.peer_addr,
                    target_addr,
                    rn,
                    wn
                );
            }
            Err(err) => {
                trace!(
                    "tcp tunnel {} <-> {} closed with error: {}",
                    self.peer_addr,
                    target_addr,
                    err
                );
            }
        }

        Ok(())
    }
}
