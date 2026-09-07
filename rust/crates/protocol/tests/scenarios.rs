//! `client.py` senaryolarının in-process (gerçek modem + gerçek kanal, GUI
//! yok) karşılıkları — CLAUDE.md "doğrulanmış senaryolar" listesi.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use channel::{ChannelConfig, ChannelCore, InProcConnector};
use protocol::{ChatScope, Role, StationConfig, StationEvent};
use tokio::sync::broadcast;

type Sta = protocol::Station<InProcConnector>;

fn fast_cfg(dir: &std::path::Path) -> StationConfig {
    // Gerçek değerlerin küçültülmüş hâli — ama beacon'ı kanalı boğmayacak
    // kadar seyrek tutuyoruz (CLAUDE.md hata #3'ün test tarafı karşılığı).
    StationConfig {
        beacon_interval: Duration::from_millis(2500),
        beacon_timeout: Duration::from_millis(6000),
        lost_timeout: Duration::from_secs(12),
        remove_timeout: Duration::from_secs(40),
        control_window_every: 3,
        control_window_pause: Duration::from_millis(250),
        received_dir: dir.to_path_buf(),
    }
}

/// Beacon/master mekanizmasını uykuya alır — transfer senaryolarını
/// election churn'ünden izole etmek için.
fn quiet_cfg(dir: &std::path::Path) -> StationConfig {
    StationConfig {
        beacon_interval: Duration::from_secs(3600),
        beacon_timeout: Duration::from_secs(3600),
        lost_timeout: Duration::from_secs(3600),
        remove_timeout: Duration::from_secs(3600),
        control_window_every: 3,
        control_window_pause: Duration::from_millis(250),
        received_dir: dir.to_path_buf(),
    }
}

async fn station(
    core: &Arc<ChannelCore>,
    call: &str,
    mode: netproto::Mode,
    cfg: StationConfig,
) -> Sta {
    protocol::Station::start(
        InProcConnector::new(Arc::clone(core), call),
        call,
        mode,
        cfg,
    )
    .await
    .unwrap()
}

async fn wait_for<F>(
    rx: &mut broadcast::Receiver<StationEvent>,
    secs: u64,
    mut pred: F,
) -> Option<StationEvent>
where
    F: FnMut(&StationEvent) -> bool,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(ev)) => {
                if pred(&ev) {
                    return Some(ev);
                }
            }
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            _ => return None,
        }
    }
}

async fn wait_role(st: &Sta, secs: u64, want: Role) -> bool {
    for _ in 0..(secs * 20) {
        if st.role() == want {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    st.role() == want
}

fn root_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(name)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn single_station_elects_itself_master() {
    let dir = tempfile::tempdir().unwrap();
    let core = ChannelCore::spawn(ChannelConfig::default());
    let a = station(&core, "TA1ABC", netproto::Mode::Qpsk, fast_cfg(dir.path())).await;

    assert_eq!(a.role(), Role::Listener);
    assert!(
        wait_role(&a, 15, Role::Master).await,
        "istasyon ~6 sn sonra MASTER olmalıydı"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_broadcast_reaches_peer_over_modem() {
    let dir = tempfile::tempdir().unwrap();
    let core = ChannelCore::spawn(ChannelConfig::default());
    let a = station(&core, "TA1ABC", netproto::Mode::Qpsk, fast_cfg(dir.path())).await;
    let b = station(&core, "TA2DEF", netproto::Mode::Qpsk, fast_cfg(dir.path())).await;
    let mut b_ev = b.subscribe();
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert!(a.chat("merhaba dunya", "ALL").await);

    let ev = wait_for(
        &mut b_ev,
        12,
        |e| matches!(e, StationEvent::Chat { text, .. } if text == "merhaba dunya"),
    )
    .await
    .expect("TA2DEF sohbeti çözüp yayınlamalıydı");
    match ev {
        StationEvent::Chat { from, scope, .. } => {
            assert_eq!(from, "TA1ABC");
            assert_eq!(scope, ChatScope::Broadcast);
        }
        _ => unreachable!(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bulk_transfer_is_bit_exact_on_clean_channel() {
    let dir = tempfile::tempdir().unwrap();
    let src_path = dir.path().join("veri.bin");
    let payload: Vec<u8> = (0..2200u32).map(|i| (i * 37 + 11) as u8).collect(); // 10 blok
    std::fs::write(&src_path, &payload).unwrap();

    let core = ChannelCore::spawn(ChannelConfig::default());
    let a = station(&core, "TA1ABC", netproto::Mode::Qpsk, fast_cfg(dir.path())).await;
    let b = station(&core, "TA2DEF", netproto::Mode::Qpsk, fast_cfg(dir.path())).await;
    let mut b_ev = b.subscribe();
    tokio::time::sleep(Duration::from_millis(500)).await;

    a.send_file(src_path, "TA2DEF");

    let saved = wait_for(&mut b_ev, 90, |e| {
        matches!(
            e,
            StationEvent::Transfer {
                done: true,
                saved_path: Some(_),
                ..
            }
        )
    })
    .await
    .expect("transfer tamamlanmalıydı");
    if let StationEvent::Transfer {
        saved_path: Some(p),
        ..
    } = saved
    {
        assert_eq!(
            std::fs::read(&p).unwrap(),
            payload,
            "kaydedilen dosya bit-birebir değil"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn arq_recovers_from_lost_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let src_path = dir.path().join("veri.bin");
    let payload: Vec<u8> = (0..1700u32).map(|i| (i * 53 + 7) as u8).collect(); // 8 blok
    std::fs::write(&src_path, &payload).unwrap();

    // Deterministik: burst 6 ve 9'u sıfırla. Burst sırası (beacon kapalı,
    // B önce başlar): 1=JOIN(B) 2=JOIN(A) 3=BULK_META 4..11=BLOCK0..7 12=BULK_END
    // -> BLOCK2 ve BLOCK5 kaybolur; BULK_END sağlam gelir -> B eksikleri ister.
    let core = ChannelCore::spawn(ChannelConfig {
        corrupt_burst_nums: vec![6, 9],
        ..Default::default()
    });
    let b = station(&core, "TA2DEF", netproto::Mode::Qpsk, quiet_cfg(dir.path())).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let a = station(&core, "TA1ABC", netproto::Mode::Qpsk, quiet_cfg(dir.path())).await;
    let mut b_ev = b.subscribe();
    tokio::time::sleep(Duration::from_secs(1)).await;

    a.send_file(src_path, "TA2DEF");

    // ARQ turu gözlenmeli.
    let saw_arq = {
        let mut a2 = a.subscribe();
        tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
            loop {
                match tokio::time::timeout(
                    deadline.saturating_duration_since(tokio::time::Instant::now()),
                    a2.recv(),
                )
                .await
                {
                    Ok(Ok(StationEvent::Log(l))) if l.contains("yeniden gönderiliyor") => {
                        return true
                    }
                    Ok(Ok(_)) => continue,
                    _ => return false,
                }
            }
        })
    };

    let saved = wait_for(&mut b_ev, 90, |e| {
        matches!(
            e,
            StationEvent::Transfer {
                done: true,
                saved_path: Some(_),
                ..
            }
        )
    })
    .await
    .expect("ARQ ile transfer tamamlanmalıydı");
    if let StationEvent::Transfer {
        saved_path: Some(p),
        ..
    } = saved
    {
        assert_eq!(std::fs::read(&p).unwrap(), payload);
    }
    assert!(
        saw_arq.await.unwrap(),
        "en az bir ARQ yeniden gönderim turu beklenirdi"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drop_and_reconnect_resumes_transfer() {
    let dir = tempfile::tempdir().unwrap();
    let src_path = dir.path().join("veri.bin");
    let payload: Vec<u8> = (0..2200u32).map(|i| (i * 29 + 5) as u8).collect(); // ~10 blok
    std::fs::write(&src_path, &payload).unwrap();

    let core = ChannelCore::spawn(ChannelConfig::default());
    let a = station(&core, "TA1ABC", netproto::Mode::Qpsk, fast_cfg(dir.path())).await;
    let b = station(&core, "TA2DEF", netproto::Mode::Qpsk, fast_cfg(dir.path())).await;
    let mut b_ev = b.subscribe();
    tokio::time::sleep(Duration::from_millis(500)).await;

    a.send_file(src_path, "TA2DEF");
    // Birkaç blok gitsin, sonra gönderende kopma.
    tokio::time::sleep(Duration::from_secs(4)).await;
    a.drop_link().await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    a.reconnect().await;

    let saved = wait_for(&mut b_ev, 150, |e| {
        matches!(
            e,
            StationEvent::Transfer {
                done: true,
                saved_path: Some(_),
                ..
            }
        )
    })
    .await
    .expect("kopma/yeniden bağlanma sonrası transfer tamamlanmalıydı");
    if let StationEvent::Transfer {
        saved_path: Some(p),
        ..
    } = saved
    {
        assert_eq!(std::fs::read(&p).unwrap(), payload);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backup_master_takes_over_after_master_drop() {
    let dir = tempfile::tempdir().unwrap();
    let core = ChannelCore::spawn(ChannelConfig::default());

    let a = station(&core, "TA1ABC", netproto::Mode::Qpsk, fast_cfg(dir.path())).await;
    assert!(
        wait_role(&a, 10, Role::Master).await,
        "TA1ABC master olmalıydı"
    );

    // Master kurulduktan SONRA B katılır -> temiz backup ataması.
    let b = station(&core, "TA2DEF", netproto::Mode::Qpsk, fast_cfg(dir.path())).await;
    assert!(
        wait_role(&b, 12, Role::Backup).await,
        "TA2DEF, master'ın beacon'ıyla BACKUP olmalıydı"
    );

    a.drop_link().await;
    assert!(
        wait_role(&b, 15, Role::Master).await,
        "TA2DEF, master düşünce devralmalıydı"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "gerçek boyut: ~75 sn (grup_gorseli.bin, 55 blok)"]
async fn full_size_image_transfer_bit_exact() {
    let dir = tempfile::tempdir().unwrap();
    let src = root_fixture("grup_gorseli.bin");
    let original = std::fs::read(&src).expect("grup_gorseli.bin kökte olmalı");

    let core = ChannelCore::spawn(ChannelConfig::default());
    let a = station(&core, "TA1ABC", netproto::Mode::Qpsk, fast_cfg(dir.path())).await;
    let b = station(&core, "TA2DEF", netproto::Mode::Qpsk, fast_cfg(dir.path())).await;
    let mut b_ev = b.subscribe();
    tokio::time::sleep(Duration::from_millis(500)).await;

    a.send_file(src, "ALL");
    let saved = wait_for(&mut b_ev, 240, |e| {
        matches!(
            e,
            StationEvent::Transfer {
                done: true,
                saved_path: Some(_),
                ..
            }
        )
    })
    .await
    .expect("tamamlanmalıydı");
    if let StationEvent::Transfer {
        saved_path: Some(p),
        ..
    } = saved
    {
        assert_eq!(std::fs::read(&p).unwrap(), original);
    }
}
