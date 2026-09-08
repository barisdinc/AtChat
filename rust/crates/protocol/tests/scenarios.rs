//! In-process equivalents of the `client.py` scenarios (real modem + real
//! channel, no GUI) — CLAUDE.md's "verified scenarios" list.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use channel::{ChannelConfig, ChannelCore, InProcConnector};
use protocol::{ChatScope, Role, StationConfig, StationEvent};
use tokio::sync::broadcast;

type Sta = protocol::Station<InProcConnector>;

fn fast_cfg(dir: &std::path::Path) -> StationConfig {
    // Shrunk-down versions of the real values — but the beacon is kept sparse
    // enough not to choke the channel (the test-side counterpart of CLAUDE.md bug #3).
    StationConfig {
        beacon_interval: Duration::from_millis(2500),
        beacon_timeout: Duration::from_millis(6000),
        lost_timeout: Duration::from_secs(12),
        remove_timeout: Duration::from_secs(40),
        control_window_every: 3,
        control_window_pause: Duration::from_millis(250),
        control_window_every_busy: 1,
        control_window_pause_busy: Duration::from_millis(350),
        control_contended_for: Duration::from_secs(2),
        received_dir: dir.to_path_buf(),
    }
}

/// Puts the beacon/master machinery to sleep — to isolate the transfer
/// scenarios from election churn.
fn quiet_cfg(dir: &std::path::Path) -> StationConfig {
    StationConfig {
        beacon_interval: Duration::from_secs(3600),
        beacon_timeout: Duration::from_secs(3600),
        lost_timeout: Duration::from_secs(3600),
        remove_timeout: Duration::from_secs(3600),
        control_window_every: 3,
        control_window_pause: Duration::from_millis(250),
        control_window_every_busy: 1,
        control_window_pause_busy: Duration::from_millis(350),
        control_contended_for: Duration::from_secs(2),
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
        "the station should be MASTER after ~6 s"
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

    assert!(a.chat("hello world", "ALL").await);

    let ev = wait_for(
        &mut b_ev,
        12,
        |e| matches!(e, StationEvent::Chat { text, .. } if text == "hello world"),
    )
    .await
    .expect("TA2DEF should have decoded and published the chat");
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
    let src_path = dir.path().join("data.bin");
    let payload: Vec<u8> = (0..2200u32).map(|i| (i * 37 + 11) as u8).collect(); // 10 blocks
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
    .expect("the transfer should have completed");
    if let StationEvent::Transfer {
        saved_path: Some(p),
        ..
    } = saved
    {
        assert_eq!(
            std::fs::read(&p).unwrap(),
            payload,
            "the saved file is not bit-exact"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn arq_recovers_from_lost_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let src_path = dir.path().join("data.bin");
    let payload: Vec<u8> = (0..1700u32).map(|i| (i * 53 + 7) as u8).collect(); // 8 blocks
    std::fs::write(&src_path, &payload).unwrap();

    // Deterministic: zero bursts 6 and 9. Burst order (beacon off, B starts
    // first): 1=JOIN(B) 2=JOIN(A) 3=BULK_META 4..11=BLOCK0..7 12=BULK_END
    // -> BLOCK2 and BLOCK5 are lost; BULK_END arrives intact -> B requests the misses.
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

    // An ARQ round should be observed.
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
                    Ok(Ok(StationEvent::Log(l))) if l.contains("resending") => return true,
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
    .expect("the transfer should have completed via ARQ");
    if let StationEvent::Transfer {
        saved_path: Some(p),
        ..
    } = saved
    {
        assert_eq!(std::fs::read(&p).unwrap(), payload);
    }
    assert!(
        saw_arq.await.unwrap(),
        "expected at least one ARQ resend round"
    );
}

/// CLAUDE.md next-step #4: adaptive modulation. A QPSK round that loses more
/// than ADAPT_DOWNSHIFT_FRAC of its blocks makes the sender fall back to BPSK
/// for the rest of the transfer; it still completes bit-exact.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qpsk_round_with_heavy_loss_downshifts_to_bpsk() {
    let dir = tempfile::tempdir().unwrap();
    let src_path = dir.path().join("data.bin");
    let payload: Vec<u8> = (0..2200u32).map(|i| (i * 29 + 5) as u8).collect(); // ~10 blocks
    std::fs::write(&src_path, &payload).unwrap();

    // Burst order (beacon off, B first): 1=JOIN(B) 2=JOIN(A) 3=BULK_META
    // 4..=BLOCK0.. -> zero bursts 4,5,6 => BLOCK0..2 lost in round 1 (3/10 = 30%
    // > 15%) -> the sender must switch to BPSK for the resend.
    let core = ChannelCore::spawn(ChannelConfig {
        corrupt_burst_nums: vec![4, 5, 6],
        ..Default::default()
    });
    let b = station(&core, "TA2DEF", netproto::Mode::Qpsk, quiet_cfg(dir.path())).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let a = station(&core, "TA1ABC", netproto::Mode::Qpsk, quiet_cfg(dir.path())).await;
    let mut a_ev = a.subscribe();
    let mut b_ev = b.subscribe();
    tokio::time::sleep(Duration::from_secs(1)).await;

    a.send_file(src_path, "TA2DEF");

    let downshift = wait_for(
        &mut a_ev,
        90,
        |e| matches!(e, StationEvent::Log(l) if l.contains("switching to BPSK")),
    )
    .await;
    assert!(
        downshift.is_some(),
        "the sender should have downshifted QPSK -> BPSK after the lossy round"
    );

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
    .expect("the transfer should still complete after the downshift");
    if let StationEvent::Transfer {
        saved_path: Some(p),
        ..
    } = saved
    {
        assert_eq!(std::fs::read(&p).unwrap(), payload);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drop_and_reconnect_resumes_transfer() {
    let dir = tempfile::tempdir().unwrap();
    let src_path = dir.path().join("data.bin");
    let payload: Vec<u8> = (0..2200u32).map(|i| (i * 29 + 5) as u8).collect(); // ~10 blocks
    std::fs::write(&src_path, &payload).unwrap();

    let core = ChannelCore::spawn(ChannelConfig::default());
    let a = station(&core, "TA1ABC", netproto::Mode::Qpsk, fast_cfg(dir.path())).await;
    let b = station(&core, "TA2DEF", netproto::Mode::Qpsk, fast_cfg(dir.path())).await;
    let mut b_ev = b.subscribe();
    tokio::time::sleep(Duration::from_millis(500)).await;

    a.send_file(src_path, "TA2DEF");
    // Let a few blocks go out, then drop the link on the sender.
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
    .expect("the transfer should complete after drop/reconnect");
    if let StationEvent::Transfer {
        saved_path: Some(p),
        ..
    } = saved
    {
        assert_eq!(std::fs::read(&p).unwrap(), payload);
    }
}

/// CLAUDE.md next-step #2: multipath had only been exercised at the modem
/// level. Here a real transfer runs end to end through the channel's multipath
/// echo — a MILD one, well inside the 8 ms guard and in the regime CLAUDE.md
/// calls "solid" — and must land bit-exact. (A stronger echo pushes sustained
/// block loss into the known BULK_END-loss ARQ stall, so it is not asserted
/// here; BPSK + a channel estimator, #5, are the real fix.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transfer_survives_multipath_within_guard() {
    let dir = tempfile::tempdir().unwrap();
    let src_path = dir.path().join("data.bin");
    let payload: Vec<u8> = (0..1300u32).map(|i| (i * 41 + 13) as u8).collect(); // ~6 blocks
    std::fs::write(&src_path, &payload).unwrap();

    // 2 ms echo at ~-18 dB — the "mild" end of CLAUDE.md's multipath notes,
    // which the modem's CP + frequency-domain differential coding rides out.
    let core = ChannelCore::spawn(ChannelConfig {
        multipath_delay_ms: 2.0,
        multipath_gain: 0.12,
        ..Default::default()
    });
    let b = station(&core, "TA2DEF", netproto::Mode::Qpsk, quiet_cfg(dir.path())).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let a = station(&core, "TA1ABC", netproto::Mode::Qpsk, quiet_cfg(dir.path())).await;
    let mut b_ev = b.subscribe();
    tokio::time::sleep(Duration::from_secs(1)).await;

    a.send_file(src_path, "TA2DEF");

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
    .expect("the transfer should complete through mild multipath");
    if let StationEvent::Transfer {
        saved_path: Some(p),
        ..
    } = saved
    {
        assert_eq!(
            std::fs::read(&p).unwrap(),
            payload,
            "the file must be bit-exact even after multipath + ARQ"
        );
    }
}

/// CLAUDE.md next-step #3: the full multi-station NET scenario over real audio,
/// end to end — 4 stations, a group (ALL) image and a private file, plus one
/// station dropping and reconnecting mid-run. Everything must converge: one
/// master and both transfers bit-exact. The two transfers run one after the
/// other, not literally at once — two big transfers on a 2.7 kHz half-duplex
/// channel starve each other, which is a channel property, not a bug.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn four_station_net_group_and_private() {
    let dir = tempfile::tempdir().unwrap();
    let core = ChannelCore::spawn(ChannelConfig::default());

    let group: Vec<u8> = (0..1000u32).map(|i| (i * 17 + 3) as u8).collect(); // ~5 blocks
    let priv_: Vec<u8> = (0..800u32).map(|i| (i * 23 + 9) as u8).collect(); // ~4 blocks
    let group_path = dir.path().join("group.bin");
    let priv_path = dir.path().join("private.bin");
    std::fs::write(&group_path, &group).unwrap();
    std::fs::write(&priv_path, &priv_).unwrap();

    // Each station writes received files to its OWN dir — several stations
    // receive the same ALL transfer, and one shared dir would race.
    let sd = |who: &str| dir.path().join(who);
    let a = station(&core, "TA1AAA", netproto::Mode::Qpsk, fast_cfg(&sd("a"))).await;
    assert!(
        wait_role(&a, 15, Role::Master).await,
        "TA1AAA should be master"
    );
    let b = station(&core, "TA2BBB", netproto::Mode::Qpsk, fast_cfg(&sd("b"))).await;
    let c = station(&core, "TA3CCC", netproto::Mode::Qpsk, fast_cfg(&sd("c"))).await;
    let d = station(&core, "TA4DDD", netproto::Mode::Qpsk, fast_cfg(&sd("d"))).await;
    let mut c_ev = c.subscribe();
    let mut d_ev = d.subscribe();

    // Let the roster settle (a couple of beacons).
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert!(
        a.snapshot().roster.len() >= 3,
        "the master should see the other three stations"
    );

    // 1) A -> ALL group image. D drops and reconnects while it is in flight.
    a.send_file(group_path, "ALL");
    tokio::time::sleep(Duration::from_secs(3)).await;
    d.drop_link().await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    d.reconnect().await;

    let g = wait_for(&mut c_ev, 150, |e| {
        matches!(e, StationEvent::Transfer { done: true, saved_path: Some(_), peer, .. } if peer == "TA1AAA")
    })
    .await
    .expect("TA3CCC should receive the group image");
    if let StationEvent::Transfer {
        saved_path: Some(p),
        ..
    } = g
    {
        assert_eq!(
            std::fs::read(&p).unwrap(),
            group,
            "group image not bit-exact"
        );
    }
    // D, back on the air, also finishes the group image (resumed the misses).
    let gd = wait_for(&mut d_ev, 120, |e| {
        matches!(e, StationEvent::Transfer { done: true, saved_path: Some(_), peer, .. } if peer == "TA1AAA")
    })
    .await
    .expect("TA4DDD should finish the group image after reconnecting");
    if let StationEvent::Transfer {
        saved_path: Some(p),
        ..
    } = gd
    {
        assert_eq!(
            std::fs::read(&p).unwrap(),
            group,
            "D's group image not bit-exact"
        );
    }

    // 2) Now B -> TA3CCC private file.
    b.send_file(priv_path, "TA3CCC");
    let pv = wait_for(&mut c_ev, 150, |e| {
        matches!(e, StationEvent::Transfer { done: true, saved_path: Some(_), peer, .. } if peer == "TA2BBB")
    })
    .await
    .expect("TA3CCC should receive the private file from TA2BBB");
    if let StationEvent::Transfer {
        saved_path: Some(p),
        ..
    } = pv
    {
        assert_eq!(
            std::fs::read(&p).unwrap(),
            priv_,
            "private file not bit-exact"
        );
    }

    assert_eq!(
        a.role(),
        Role::Master,
        "TA1AAA should still be the one master"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backup_master_takes_over_after_master_drop() {
    let dir = tempfile::tempdir().unwrap();
    let core = ChannelCore::spawn(ChannelConfig::default());

    let a = station(&core, "TA1ABC", netproto::Mode::Qpsk, fast_cfg(dir.path())).await;
    assert!(
        wait_role(&a, 10, Role::Master).await,
        "TA1ABC should be master"
    );

    // B joins AFTER the master is established -> a clean backup assignment.
    let b = station(&core, "TA2DEF", netproto::Mode::Qpsk, fast_cfg(dir.path())).await;
    assert!(
        wait_role(&b, 12, Role::Backup).await,
        "TA2DEF should become BACKUP from the master's beacon"
    );

    a.drop_link().await;
    assert!(
        wait_role(&b, 15, Role::Master).await,
        "TA2DEF should take over when the master drops"
    );
}

/// CLAUDE.md next-step #1: failover must not stop at a single backup. Master
/// and backup both go away -> the next station in the succession line takes over.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failover_chains_past_a_single_backup() {
    let dir = tempfile::tempdir().unwrap();
    let core = ChannelCore::spawn(ChannelConfig::default());

    let a = station(&core, "TA1ABC", netproto::Mode::Qpsk, fast_cfg(dir.path())).await;
    assert!(
        wait_role(&a, 10, Role::Master).await,
        "TA1ABC should be master"
    );

    let b = station(&core, "TA2DEF", netproto::Mode::Qpsk, fast_cfg(dir.path())).await;
    let c = station(&core, "TA3GHI", netproto::Mode::Qpsk, fast_cfg(dir.path())).await;
    assert!(
        wait_role(&b, 12, Role::Backup).await,
        "TA2DEF (lowest callsign) should be BACKUP"
    );

    // First hop: master drops, the backup takes over.
    a.drop_link().await;
    assert!(
        wait_role(&b, 15, Role::Master).await,
        "TA2DEF should take over from TA1ABC"
    );

    // Second hop: the NEW master also drops. The old logic left the net with
    // no master here; now TA3GHI should climb the succession line and claim it.
    b.drop_link().await;
    assert!(
        wait_role(&c, 25, Role::Master).await,
        "TA3GHI should take over once the backup is gone too"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "full size: ~75 s (grup_gorseli.bin, 55 blocks)"]
async fn full_size_image_transfer_bit_exact() {
    let dir = tempfile::tempdir().unwrap();
    let src = root_fixture("grup_gorseli.bin");
    let original = std::fs::read(&src).expect("grup_gorseli.bin must be at the repo root");

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
    .expect("should have completed");
    if let StationEvent::Transfer {
        saved_path: Some(p),
        ..
    } = saved
    {
        assert_eq!(std::fs::read(&p).unwrap(), original);
    }
}
