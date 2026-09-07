//! `channel_server.py::handle_client` portu — TCP soketlerini [`ChannelCore`]'a
//! köprüler. Tel formatı `channel_server.py` ile birebir olduğundan Python
//! `client.py` / `monitor.py` bu sunucuya bağlanabilir.

use std::sync::Arc;

use anyhow::{anyhow, bail};
use tokio::io::{BufReader, BufWriter};
use tokio::net::{TcpListener, TcpStream};

use crate::core::ChannelCore;
use crate::link::b64_to_samples;

pub async fn serve(listener: TcpListener, core: Arc<ChannelCore>) -> anyhow::Result<()> {
    loop {
        let (stream, peer) = listener.accept().await?;
        stream.set_nodelay(true).ok();
        let core = Arc::clone(&core);
        tokio::spawn(async move {
            if let Err(e) = handle_conn(stream, core).await {
                tracing::debug!("bağlantı bitti ({peer}): {e}");
            }
        });
    }
}

async fn handle_conn(stream: TcpStream, core: Arc<ChannelCore>) -> anyhow::Result<()> {
    let (r, w) = stream.into_split();
    let mut reader = BufReader::new(r);
    let mut writer = BufWriter::new(w);

    let hello = netproto::read_json(&mut reader)
        .await?
        .ok_or_else(|| anyhow!("HELLO'dan önce EOF"))?;
    if hello.get("cmd").and_then(|v| v.as_str()) != Some("HELLO") {
        bail!("ilk mesaj HELLO değil");
    }
    let callsign = hello
        .get("callsign")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("HELLO içinde callsign yok"))?
        .to_string();

    let (id, mut rx) = core.register(&callsign);

    // core -> soket
    let writer_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if netproto::write_json(&mut writer, &msg).await.is_err() {
                break;
            }
        }
    });

    // soket -> core
    let pump: anyhow::Result<()> = async {
        loop {
            let Some(v) = netproto::read_json(&mut reader).await? else {
                break;
            };
            if v.get("cmd").and_then(|c| c.as_str()) == Some("TRANSMIT_AUDIO") {
                if let Some(b64) = v.get("audio_b64").and_then(|a| a.as_str()) {
                    let samples = b64_to_samples(b64)?;
                    core.transmit(id, samples);
                }
            }
        }
        Ok(())
    }
    .await;

    core.deregister(id);
    writer_task.abort();
    pump
}
