use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::sync::{Mutex, mpsc, oneshot};
use anyhow::Result;
use log::{debug, error, info};
use crate::tunnel::tunnel::Tunnel;
use tokio::time::{timeout, Duration};


const TCP_WRITE_TIMEOUT: u64 = 3;

pub struct TcpProxy {
    pub id: String,
    reader: Option<Mutex<ReadHalf<TcpStream>>>,
    raw: Option<Arc<Mutex<TcpStream>>>,
    write_queue: mpsc::UnboundedSender<Vec<u8>>,
    // 用于通知写入处理器连接已就绪
    writer_ready_tx: Option<oneshot::Sender<Arc<Mutex<WriteHalf<TcpStream>>>>>,
}

impl TcpProxy {
    /// 创建 TcpProxy，立即可以接收写入数据（0-RTT）
    pub fn new_with_queue(id: String) -> Self {
        // 创建写入队列（立即可用）
        let (write_tx, write_rx) = mpsc::unbounded_channel();
        
        // 创建 writer 就绪通知通道
        let (writer_ready_tx, writer_ready_rx) = oneshot::channel();
        
        let proxy = Self {
            id: id.clone(),
            reader: None,
            raw: None,
            write_queue: write_tx,
            writer_ready_tx: Some(writer_ready_tx),
        };
        
        // 启动写入队列处理器（等待 writer 就绪）
        let proxy_id = id.clone();
        tokio::spawn(async move {
            Self::write_queue_processor(proxy_id, writer_ready_rx, write_rx).await;
        });
        
        proxy
    }
    
    /// 设置 TCP 连接（连接建立后调用）
    pub async fn set_connection(mut self, stream: TcpStream) -> Result<Self> {
        // 从 tokio stream 提取同步版本
        let std_stream = stream.into_std()?;
        let std_stream_clone = std_stream.try_clone()?;
        let raw = TcpStream::from_std(std_stream_clone)?;
        let stream = TcpStream::from_std(std_stream)?;

        let (reader, writer) = tokio::io::split(stream);
        
        self.reader = Some(Mutex::new(reader));
        self.raw = Some(Arc::new(Mutex::new(raw)));
        
        // 通知写入处理器 writer 已就绪
        let writer_arc = Arc::new(Mutex::new(writer));
        if let Some(tx) = self.writer_ready_tx.take() {
            let _ = tx.send(writer_arc);
            info!("tcp proxy {} connection ready, write queue can now flush", self.id);
        }
        
        Ok(self)
    }
    
    /// 写入队列处理器
    async fn write_queue_processor(
        id: String,
        writer_ready_rx: oneshot::Receiver<Arc<Mutex<WriteHalf<TcpStream>>>>,
        mut write_rx: mpsc::UnboundedReceiver<Vec<u8>>
    ) {
        // 等待 writer 就绪
        let writer = match writer_ready_rx.await {
            Ok(w) => {
                info!("tcp proxy {} write queue processor: writer ready, start processing buffered data", id);
                w
            }
            Err(_) => {
                error!("tcp proxy {} write queue processor: writer channel closed before ready", id);
                return;
            }
        };
        
        // 开始处理队列中的数据（包括之前缓冲的数据）
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
        // 确保 reader 已就绪
        let reader = match &self.reader {
            Some(r) => r,
            None => {
                error!("tcp proxy {} proxy_conn called but reader not ready", self.id);
                return;
            }
        };
        
        let mut buf = [0u8; 4096];
        loop {
            let n = {
                let mut reader_guard = reader.lock().await;
                match reader_guard.read(&mut buf).await {
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
    }

    pub async fn write(&self, data: &[u8]) -> Result<()> {
        // 非阻塞：直接发送到队列
        self.write_queue
            .send(data.to_vec())
            .map_err(|e| anyhow::anyhow!("write queue send failed: {}", e))?;
        Ok(())
    }

    async fn shutdown(&self) {
        if let Some(raw) = &self.raw {
            let mut raw_guard = raw.lock().await;
            let _ = raw_guard.shutdown().await;
        }
    }

    pub async fn close_by_server(&self) {
        self.shutdown().await;
    }

    pub async fn destroy(&self) {
        self.shutdown().await;
    }
}
