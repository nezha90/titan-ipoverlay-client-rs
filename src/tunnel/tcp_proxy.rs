use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::sync::{Mutex, mpsc};
use anyhow::Result;
use log::{debug, error};
use crate::tunnel::tunnel::Tunnel;
use tokio::time::{timeout, Duration};


const TCP_WRITE_TIMEOUT: u64 = 3;

pub struct TcpProxy {
    pub id: String,
    reader: Mutex<ReadHalf<TcpStream>>,
    writer: Arc<Mutex<WriteHalf<TcpStream>>>,
    raw: Arc<Mutex<TcpStream>>, // ✅ 保留原始连接，供 shutdown 使用
    write_queue: mpsc::UnboundedSender<Vec<u8>>,
}

impl TcpProxy {
    pub async fn new(id: String, stream: TcpStream) -> Result<Self> {
        // 先从 tokio stream 提取同步版本
        let std_stream = stream.into_std()?;
        // 克隆出一个完整的 stream 以备 shutdown 使用
        let std_stream_clone = std_stream.try_clone()?;
        // 再转换回 tokio 异步版本
        let raw = TcpStream::from_std(std_stream_clone)?;
        let stream = TcpStream::from_std(std_stream)?; // 原 stream 再恢复回异步版

        let (reader, writer) = tokio::io::split(stream);
        
        // 创建写入队列
        let (write_tx, write_rx) = mpsc::unbounded_channel();
        
        let proxy = Self {
            id: id.clone(),
            reader: Mutex::new(reader),
            writer: Arc::new(Mutex::new(writer)),
            raw: Arc::new(Mutex::new(raw)),
            write_queue: write_tx,
        };
        
        // 启动写入队列处理器
        let writer_arc = proxy.writer.clone();
        let proxy_id = id.clone();
        tokio::spawn(async move {
            Self::write_queue_processor(proxy_id, writer_arc, write_rx).await;
        });
        
        Ok(proxy)
    }
    
    /// 写入队列处理器
    async fn write_queue_processor(
        id: String,
        writer: Arc<Mutex<WriteHalf<TcpStream>>>,
        mut write_rx: mpsc::UnboundedReceiver<Vec<u8>>
    ) {
        while let Some(data) = write_rx.recv().await {
            let mut writer_guard = writer.lock().await;
            let result = timeout(
                Duration::from_secs(TCP_WRITE_TIMEOUT),
                writer_guard.write_all(&data)
            ).await;
            
            match result {
                Ok(Ok(())) => {
                    // 写入成功
                }
                Ok(Err(e)) => {
                    error!("tcp proxy {} write failed: {}", id, e);
                    break;
                }
                Err(_) => {
                    error!("tcp proxy {} write timeout", id);
                    break;
                }
            }
        }
        debug!("tcp proxy {} write queue processor exited", id);
    }

    pub async fn proxy_conn(self: Arc<Self>, tunnel: Arc<Tunnel>) {
        let mut buf = [0u8; 4096];
        loop {
            let n = {
                let mut reader = self.reader.lock().await;
                match reader.read(&mut buf).await {
                    Ok(0) => {
                        debug!("tcp proxy read eof id={}", self.id);
                        break;
                    }
                    Ok(n) => n,
                    Err(e) => {
                        error!("tcp proxy read err id={} err={:?}", self.id, e);
                        break;
                    }
                }
            };
            if let Err(e) = tunnel.on_proxy_session_data_from_proxy(&self.id, &buf[..n]).await {
                log::error!("on_proxy_session_data_from_proxy error: {}", e);
            }
        }
         if let Err(e)  = tunnel.on_proxy_conn_close(&self.id).await {
            log::error!("on_proxy_conn_close error: {}", e);
         }
        // self.shutdown().await;
    }

    pub async fn write(&self, data: &[u8]) -> Result<()> {
        // 非阻塞：直接发送到队列
        self.write_queue
            .send(data.to_vec())
            .map_err(|e| anyhow::anyhow!("write queue send failed: {}", e))?;
        Ok(())
    }

    async fn shutdown(&self) {
        let mut raw = self.raw.lock().await;
        let _ = raw.shutdown().await;
    }

    pub async fn close_by_server(&self) {
        self.shutdown().await;
    }

    pub async fn destroy(&self) {
        self.shutdown().await;
    }
}
