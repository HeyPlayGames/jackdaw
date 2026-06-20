//! Chunked scene transfer from the editor over PIE IPC.
//!
//! The editor sends [`ControlEvent::LoadSceneBegin`], one or more
//! [`ControlEvent::LoadSceneChunk`] messages, then [`ControlEvent::LoadSceneEnd`].
//! This module assembles the JSON payload, inserts or updates a [`JackdawScene`]
//! asset, and exposes [`PieSceneReady`] for games to spawn from.

use std::path::{PathBuf};

use bevy::prelude::*;
use jackdaw_jsn::format::{JsnScene, JsnSceneV2};
use jackdaw_pie_protocol::event::{PieChannel, StateEvent, to_bytes};
use jackdaw_pie_protocol::{ControlEvent, PieTransport};

use crate::pie::PieTransportRes;
use crate::JackdawScene;

/// Handle and session id for the most recently loaded PIE scene snapshot.
///
/// Games poll this resource (or watch `session` for hot reload) instead of
/// reading scene files from disk.
#[derive(Resource, Clone, Debug)]
pub struct PieSceneReady {
    pub handle: Handle<JackdawScene>,
    pub session: u64,
}

struct ActiveReceive {
    session: u64,
    parent_path: PathBuf,
    source_label: Option<String>,
    buffer: Vec<u8>,
    expected_len: usize,
}

#[derive(Resource, Default)]
pub(crate) struct PieSceneTransfer {
    active: Option<ActiveReceive>,
    scene_handle: Option<Handle<JackdawScene>>,
}

pub(crate) fn init_pie_scene_transfer(app: &mut App) {
    app.init_resource::<PieSceneTransfer>();
}

pub(crate) fn apply_load_scene_control(world: &mut World, event: ControlEvent) {
    match event {
        ControlEvent::LoadSceneBegin {
            session,
            parent_path,
            byte_len,
            source_label,
        } => {
            let Ok(expected_len) = usize::try_from(byte_len) else {
                warn!("PIE: LoadSceneBegin session {session} byte_len {byte_len} exceeds usize");
                return;
            };
            let mut transfer = world.resource_mut::<PieSceneTransfer>();
            transfer.active = Some(ActiveReceive {
                session,
                parent_path,
                source_label,
                buffer: vec![0; expected_len],
                expected_len,
            });
            info!("PIE: receiving scene session {session} ({byte_len} bytes)");
        }
        ControlEvent::LoadSceneChunk {
            session,
            offset,
            bytes,
        } => {
            let Some(mut transfer) = world.get_resource_mut::<PieSceneTransfer>() else {
                warn!("PIE: LoadSceneChunk session {session} with no transfer resource");
                return;
            };
            let Some(active) = transfer.active.as_mut() else {
                warn!("PIE: LoadSceneChunk session {session} with no active transfer");
                return;
            };
            if active.session != session {
                warn!(
                    "PIE: LoadSceneChunk session {session} does not match active {}",
                    active.session
                );
                return;
            }
            let Ok(start) = usize::try_from(offset) else {
                warn!("PIE: LoadSceneChunk offset {offset} exceeds usize");
                return;
            };
            let end = start.saturating_add(bytes.len());
            if end > active.expected_len {
                warn!(
                    "PIE: LoadSceneChunk session {session} overruns buffer ({end} > {})",
                    active.expected_len
                );
                return;
            }
            active.buffer[start..end].copy_from_slice(&bytes);
        }
        ControlEvent::LoadSceneEnd { session } => {
            finish_scene_transfer(world, session);
        }
        _ => {}
    }
}

fn finish_scene_transfer(world: &mut World, session: u64) {
    let Some(active) = world
        .resource_mut::<PieSceneTransfer>()
        .active
        .take()
        .filter(|active| active.session == session)
    else {
        warn!("PIE: LoadSceneEnd session {session} with no matching active transfer");
        return;
    };

    let text = match std::str::from_utf8(&active.buffer) {
        Ok(text) => text,
        Err(error) => {
            warn!("PIE: scene session {session} is not valid UTF-8: {error}");
            return;
        }
    };

    let jsn = match parse_jsn_scene(text) {
        Ok(jsn) => jsn,
        Err(error) => {
            warn!("PIE: scene session {session} failed to parse: {error}");
            return;
        }
    };

    let stem = active
        .source_label
        .filter(|label| !label.is_empty());
    let scene = JackdawScene {
        jsn,
        parent_path: active.parent_path,
        stem,
    };

    let existing_handle = world
        .resource::<PieSceneTransfer>()
        .scene_handle
        .clone();

    let handle = if let Some(existing) = existing_handle {
        if let Err(error) = world
            .resource_mut::<Assets<JackdawScene>>()
            .insert(existing.id(), scene)
        {
            warn!("PIE: failed to update scene asset {:?}: {error}", existing.id());
        }
        existing
    } else {
        let handle = world.resource_mut::<Assets<JackdawScene>>().add(scene);
        world
            .resource_mut::<PieSceneTransfer>()
            .scene_handle = Some(handle.clone());
        handle
    };

    world.insert_resource(PieSceneReady {
        handle: handle.clone(),
        session,
    });
    info!("PIE: scene session {session} loaded into asset {handle:?}");

    if world.contains_non_send::<PieTransportRes>() {
        if let Ok(bytes) = to_bytes(&StateEvent::SceneLoaded { session }) {
            world
                .non_send_resource_mut::<PieTransportRes>()
                .0
                .send(PieChannel::Reliable, &bytes);
        }
    }
}

fn parse_jsn_scene(text: &str) -> Result<JsnScene, String> {
    match serde_json::from_str::<JsnScene>(text) {
        Ok(jsn) => Ok(jsn),
        Err(v3_error) => match serde_json::from_str::<JsnSceneV2>(text) {
            Ok(v2) => Ok(v2.migrate_to_v3()),
            Err(_) => Err(v3_error.to_string()),
        },
    }
}
