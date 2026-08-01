//! PC/SC reader monitor — DETECTION ONLY, by hard boundary.
//!
//! This module never connects to a card and never transmits anything: no
//! SCardConnect, no SCardTransmit, no APDUs (not even the Get-UID pseudo-APDU).
//! It watches SCardGetStatusChange for card present/absent edges and
//! re-enumerates readers between waits (≤1.5s plug/unplug latency — well
//! under the operator's walk-to-station time; the PNP_NOTIFICATION magic
//! reader is deliberately avoided, its handshake is a hot-loop trap).
//!
//! Chip identity is NOT observable here — it arrives with the keying layer's
//! result (see seams/keying.rs and SEAM.md).
//!
//! Tap semantics are edge-triggered: EMPTY → PRESENT fires one presentation,
//! and the card must return to EMPTY before the next one counts. A card
//! already on the reader at monitor start does NOT fire.

use crate::core::actor::CoreHandle;
use crate::core::messages::CoreMsg;
use pcsc::{Context, Error as PcscError, ReaderState, Scope, State};
use std::collections::HashMap;
use std::ffi::CString;
use std::time::Duration;

pub fn spawn(core: CoreHandle, reader_match: String) {
    std::thread::Builder::new()
        .name("pcsc-monitor".into())
        .spawn(move || monitor_loop(core, reader_match))
        .expect("spawn pcsc monitor thread");
}

fn monitor_loop(core: CoreHandle, reader_match: String) {
    let mut reported_present: Option<bool> = None;
    loop {
        let context = match Context::establish(Scope::User) {
            Ok(ctx) => ctx,
            Err(e) => {
                tracing::warn!("pcsc context unavailable ({e}); retrying in 2s");
                report_presence(&core, &mut reported_present, false);
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        tracing::info!("pcsc context established");
        if let Err(e) = watch(&core, &context, &reader_match, &mut reported_present) {
            tracing::warn!("pcsc monitor error ({e}); re-establishing");
            report_presence(&core, &mut reported_present, false);
            std::thread::sleep(Duration::from_secs(1));
        }
    }
}

fn report_presence(core: &CoreHandle, reported: &mut Option<bool>, present: bool) {
    if *reported != Some(present) {
        *reported = Some(present);
        tracing::info!("reader {}", if present { "present" } else { "absent" });
        core.blocking_send(CoreMsg::ReaderPresence { present });
    }
}

fn watch(
    core: &CoreHandle,
    context: &Context,
    reader_match: &str,
    reported_present: &mut Option<bool>,
) -> Result<(), PcscError> {
    // Per-reader card state: Some(true)=card present, Some(false)=empty.
    // A reader's FIRST observation only records — it never fires a tap.
    let mut card_state: HashMap<CString, bool> = HashMap::new();
    let mut watched: Vec<CString> = Vec::new();
    let mut states: Vec<ReaderState> = Vec::new();

    loop {
        let mut readers_buf = [0u8; 4096];
        let matched: Vec<CString> = context
            .list_readers(&mut readers_buf)?
            .filter(|name| name.to_string_lossy().contains(reader_match))
            .map(|name| name.to_owned())
            .collect();

        report_presence(core, reported_present, !matched.is_empty());
        if matched.len() > 1 && watched.len() <= 1 {
            tracing::warn!(
                "{} readers match this station's filter — one reader per station; \
                 taps from ALL of them will drive the line",
                matched.len()
            );
        }

        if matched != watched {
            card_state.retain(|name, _| matched.contains(name));
            states = matched
                .iter()
                .map(|name| ReaderState::new(name.clone(), State::UNAWARE))
                .collect();
            watched = matched;
        }
        if states.is_empty() {
            std::thread::sleep(Duration::from_millis(1200));
            continue;
        }

        match context.get_status_change(Duration::from_millis(1500), &mut states) {
            Ok(()) | Err(PcscError::Timeout) => {}
            Err(e) => return Err(e),
        }

        for rs in states.iter_mut() {
            let name = rs.name().to_owned();
            let present = rs.event_state().intersects(State::PRESENT);
            match card_state.get(&name) {
                None => {
                    // First observation — record silently (a card already
                    // lying on the reader at start must not key anything).
                    card_state.insert(name, present);
                }
                Some(false) if present => {
                    card_state.insert(name.clone(), true);
                    tracing::info!("chip presented on {:?}", name.to_string_lossy());
                    core.blocking_send(CoreMsg::CardPresented {
                        reader_id: name.to_string_lossy().into_owned(),
                        // Passive bytes from the status change itself — the
                        // monitor still never connects or transmits.
                        atr: rs.atr().to_vec(),
                        forced: None,
                    });
                }
                Some(_) => {
                    card_state.insert(name, present);
                }
            }
            rs.sync_current_state();
        }
    }
}
