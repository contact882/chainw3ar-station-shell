//! `--reader-probe` — guided hardware verification of the detection path
//! against the REAL reader. No windows, no webview, no state: just the
//! actual pcsc monitor thread feeding the actual CoreMsg channel, with a
//! judge that scores the four cases from the hardware gate:
//!
//!   1. no card            → correct idle (no phantom presentations)
//!   2. card presented     → fires exactly once, not repeatedly
//!   3. card removed       → clean return to idle; edge re-arms
//!   4. reader unplugged   → graceful degrade, monitor survives, replug recovers
//!
//! Detection-only guarantees hold here identically to production: the
//! monitor never connects to a card and never transmits. Console output
//! reports ATR byte LENGTH only — never the bytes.

use crate::core::messages::CoreMsg;
use std::time::Duration;
use tokio::sync::mpsc::Receiver;
use tokio::time::{timeout, Instant};

pub fn run(reader_match: String) -> i32 {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(async move {
        let (handle, rx) = crate::core::actor::channel();
        crate::reader::spawn(handle, reader_match);
        judge(rx).await
    })
}

struct Probe {
    rx: Receiver<CoreMsg>,
    failures: u32,
}

enum Seen {
    Presence(bool),
    Presentation(usize), // atr byte length
    Timeout,
}

impl Probe {
    async fn next(&mut self, window: Duration) -> Seen {
        let deadline = Instant::now() + window;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match timeout(remaining, self.rx.recv()).await {
                Err(_) => return Seen::Timeout,
                Ok(None) => return Seen::Timeout, // monitor gone — caller judges
                Ok(Some(CoreMsg::ReaderPresence { present })) => return Seen::Presence(present),
                Ok(Some(CoreMsg::CardPresented { atr, .. })) => return Seen::Presentation(atr.len()),
                Ok(Some(_)) => continue,
            }
        }
    }

    fn verdict(&mut self, pass: bool, label: &str, detail: &str) {
        if !pass {
            self.failures += 1;
        }
        println!("  {}  {label} — {detail}", if pass { "PASS" } else { "FAIL" });
    }

    /// Absorb events for `window`, returning (presentations, last_presence).
    async fn absorb(&mut self, window: Duration) -> (u32, Option<bool>) {
        let mut presentations = 0;
        let mut presence = None;
        let deadline = Instant::now() + window;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return (presentations, presence);
            }
            match self.next(remaining).await {
                Seen::Presentation(_) => presentations += 1,
                Seen::Presence(p) => presence = Some(p),
                Seen::Timeout => return (presentations, presence),
            }
        }
    }
}

async fn judge(rx: Receiver<CoreMsg>) -> i32 {
    let mut probe = Probe { rx, failures: 0 };
    println!("\nreader probe — detection path vs real hardware (zero transmits)");
    println!("Start with the reader plugged in and NO card on it.\n");

    // ── Case 1: idle ────────────────────────────────────────────────────────
    println!("[1/4] idle check…");
    match probe.next(Duration::from_secs(8)).await {
        Seen::Presence(true) => {
            let (phantom, _) = probe.absorb(Duration::from_secs(5)).await;
            probe.verdict(phantom == 0, "no card → idle", if phantom == 0 {
                "reader visible, no phantom presentations in 5s"
            } else {
                "phantom presentation with no card (is a card resting on the reader?)"
            });
        }
        Seen::Presence(false) | Seen::Timeout => {
            probe.verdict(false, "no card → idle", "reader not visible — plug it in and rerun");
            println!("\nPROBE: FAIL (1 case, 3 not attempted)");
            return 1;
        }
        Seen::Presentation(_) => {
            probe.verdict(false, "no card → idle", "presentation before presence — remove the card and rerun");
        }
    }

    // ── Case 2: single fire ────────────────────────────────────────────────
    println!("[2/4] PRESENT a card now and LEAVE it on the reader (30s)…");
    match probe.next(Duration::from_secs(30)).await {
        Seen::Presentation(atr_len) => {
            println!("       presentation observed (atr {atr_len} bytes — length only, bytes stay internal)");
            let (extra, _) = probe.absorb(Duration::from_secs(5)).await;
            probe.verdict(
                extra == 0,
                "card presented → fires once",
                if extra == 0 { "no re-fire while the card sat on the reader for 5s" } else { "re-fired while held" },
            );
        }
        _ => probe.verdict(false, "card presented → fires once", "no presentation within 30s"),
    }

    // ── Case 3: removal + re-arm ───────────────────────────────────────────
    println!("[3/4] REMOVE the card now; probe watches 8s for a clean transition…");
    let (during_removal, presence) = probe.absorb(Duration::from_secs(8)).await;
    probe.verdict(
        during_removal == 0 && presence != Some(false),
        "card removed → clean idle",
        "no events, no presence flap on removal",
    );
    println!("       now PRESENT the card once more, briefly (30s)…");
    match probe.next(Duration::from_secs(30)).await {
        Seen::Presentation(_) => {
            let (extra, _) = probe.absorb(Duration::from_secs(4)).await;
            probe.verdict(extra == 0, "edge re-arms after removal", "exactly one new presentation");
        }
        _ => probe.verdict(false, "edge re-arms after removal", "no presentation within 30s"),
    }

    // ── Case 4: unplug mid-session ─────────────────────────────────────────
    println!("[4/4] UNPLUG the reader now (30s)…");
    let mut unplug_seen = false;
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        match probe.next(deadline - Instant::now()).await {
            Seen::Presence(false) => {
                unplug_seen = true;
                break;
            }
            Seen::Timeout => break,
            _ => continue,
        }
    }
    probe.verdict(unplug_seen, "unplug → graceful degrade", if unplug_seen {
        "reader-absent reported; monitor alive, no crash"
    } else {
        "no reader-absent signal within 30s"
    });
    if unplug_seen {
        println!("       REPLUG it (60s)…");
        let mut replug_seen = false;
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            match probe.next(deadline - Instant::now()).await {
                Seen::Presence(true) => {
                    replug_seen = true;
                    break;
                }
                Seen::Timeout => break,
                _ => continue,
            }
        }
        probe.verdict(replug_seen, "replug → recovers", if replug_seen {
            "reader-present restored without restart"
        } else {
            "no recovery within 60s"
        });
    }

    let code = if probe.failures == 0 { 0 } else { 1 };
    println!(
        "\nPROBE: {} ({} failure{})",
        if code == 0 { "PASS" } else { "FAIL" },
        probe.failures,
        if probe.failures == 1 { "" } else { "s" },
    );
    code
}
