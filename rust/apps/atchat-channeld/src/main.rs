//! Headless TCP kanal sunucusu. `channel_server.py` ile aynı komut satırı
//! ve aynı tel formatı — Python `client.py` / `monitor.py` doğrudan bağlanır.
//!
//!   atchat-channeld --port 6000
//!   atchat-channeld --port 6000 --snr 13 --multipath-delay-ms 3 --multipath-gain 0.2

use std::sync::Arc;

use channel::{ChannelConfig, ChannelCore, ChannelEvent};
use clap::Parser;
use tokio::net::TcpListener;

#[derive(Parser, Debug)]
#[command(about = "NET kanal simülatörü (gerçek ses sürümü) — Rust portu")]
struct Args {
    #[arg(long, default_value_t = 6000)]
    port: u16,

    /// AWGN gürültü seviyesi (dB). Verilmezse gürültü eklenmez (temiz kanal).
    #[arg(long)]
    snr: Option<f64>,

    /// Çoklu-yol yankısının gecikmesi (ms). Koruma aralığı 8 ms.
    #[arg(long = "multipath-delay-ms", default_value_t = 0.0)]
    multipath_delay_ms: f64,

    /// Yankının doğrudan sinyale göre kazancı (0–1, ör. 0.3).
    #[arg(long = "multipath-gain", default_value_t = 0.0)]
    multipath_gain: f64,
}

fn now() -> String {
    // basit HH:MM:SS — chrono bağımlılığı eklememek için.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    format!("{h:02}:{m:02}:{s:02}")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();
    let cfg = ChannelConfig {
        snr_db: args.snr,
        multipath_delay_ms: args.multipath_delay_ms,
        multipath_gain: args.multipath_gain,
        ..Default::default()
    };
    let core = ChannelCore::spawn(cfg);

    let listener = TcpListener::bind(("127.0.0.1", args.port)).await?;
    println!(
        "[KANAL {}] dinleniyor: 127.0.0.1:{}  (snr={:?}, multipath={}ms@{})",
        now(),
        args.port,
        args.snr,
        args.multipath_delay_ms,
        args.multipath_gain
    );

    let mut ev = core.subscribe_events();
    tokio::spawn(async move {
        loop {
            match ev.recv().await {
                Ok(e) => log_event(&e),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    });

    let serve = channel::tcp_server::serve(listener, Arc::clone(&core));
    tokio::select! {
        r = serve => r,
        _ = tokio::signal::ctrl_c() => {
            println!("\n[KANAL {}] kapatılıyor", now());
            Ok(())
        }
    }
}

fn log_event(e: &ChannelEvent) {
    let t = now();
    match e {
        ChannelEvent::Joined { callsign, active } => {
            println!("[KANAL {t}] {callsign} bağlandı ({active} istasyon aktif)")
        }
        ChannelEvent::Left { callsign, active } => {
            println!("[KANAL {t}] {callsign} bağlantısı koptu ({active} istasyon aktif)")
        }
        ChannelEvent::TxGranted {
            src,
            n_samples,
            duration,
        } => println!(
            "[KANAL {t}] {src:8} -> ALL      | ses | {n_samples:6} örnek | süre={duration:.2}sn"
        ),
        ChannelEvent::TxDenied { src, retry_after } => {
            println!("[KANAL {t}] {src:8} -> MEŞGUL   | retry_after={retry_after:.2}sn")
        }
        ChannelEvent::Decoded { duration, summary } => match summary {
            Some(s) => println!("[DİNLE {t}] {s} | {duration:.2}sn | çözüldü"),
            None => println!("[DİNLE {t}] {duration:.2}sn | çözülemedi"),
        },
        ChannelEvent::Delivered { .. } => {}
    }
}
