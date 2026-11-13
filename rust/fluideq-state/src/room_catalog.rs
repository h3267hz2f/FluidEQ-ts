/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! The built-in rooms the Room card offers, ported from the upstream
//! `roomPresets.ts` catalogue: eleven classic rooms on the first renderer and
//! thirteen featured rooms on the second.
//!
//! A preset sets the whole room — shape, speaker angles and levels, the
//! listener's distance, bass management, the stereo fill and the renderer's
//! space and tail — and leaves the head, the power switch and the two choices
//! about what is playing to the listener.
//!
//! `index` is the wire's preset number, fixed for good: the classic rooms and
//! `custom` occupy 0–11, the featured rooms follow from 12. The picker order
//! (`catalog`) is a separate thing, grouped featured-first then classic.

use fluideq_dsp_sys::FeqChainRoomSettings;
use serde::Serialize;

pub const ROOM_SPEAKERS: usize = 7;
pub const ROOM_MUTE_SLOTS: usize = 8;

/// The wire's number for a room that no longer matches any preset: what a
/// saved room becomes once a dial, a speaker or its bass management is moved.
pub const CUSTOM_PRESET: i32 = 11;

const RING: [f64; ROOM_SPEAKERS] = [-30.0, 30.0, 0.0, -100.0, 100.0, -140.0, 140.0];
const FLAT: [f64; ROOM_SPEAKERS] = [0.0; ROOM_SPEAKERS];
const MUTES: [i32; ROOM_MUTE_SLOTS] = [0; ROOM_MUTE_SLOTS];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RoomCollection {
    Featured,
    Classic,
}

impl RoomCollection {
    pub fn as_str(self) -> &'static str {
        match self {
            RoomCollection::Featured => "featured",
            RoomCollection::Classic => "classic",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RoomPresetGroup {
    Basic,
    Playback,
    Character,
}

impl RoomPresetGroup {
    pub fn as_str(self) -> &'static str {
        match self {
            RoomPresetGroup::Basic => "basic",
            RoomPresetGroup::Playback => "playback",
            RoomPresetGroup::Character => "character",
        }
    }
}

/// Every field a room preset owns. The head, `enabled`, `compare_original`
/// and `source_already_spatial` are deliberately absent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RoomShape {
    pub size_m: f64,
    pub walls: f64,
    pub distance_m: f64,
    pub centre_db: f64,
    pub sub_db: f64,
    pub angles: [f64; ROOM_SPEAKERS],
    pub levels: [f64; ROOM_SPEAKERS],
    pub distances: [f64; ROOM_SPEAKERS],
    pub mutes: [i32; ROOM_MUTE_SLOTS],
    pub bass_management: bool,
    pub crossover_hz: f64,
    pub music_upmix: bool,
    pub upmix_amount: f64,
    pub renderer_version: i32,
    pub early_reflection_db: f64,
    pub ambience_mix: f64,
    pub ambience_decay_s: f64,
    pub ambience_damping_hz: f64,
    pub preserve_position: bool,
}

/// A classic room: the seven numbers, on the first renderer, with the card's
/// default bass and stereo handling.
const fn classic(
    size_m: f64,
    walls: f64,
    distance_m: f64,
    centre_db: f64,
    sub_db: f64,
    angles: [f64; ROOM_SPEAKERS],
    levels: [f64; ROOM_SPEAKERS],
) -> RoomShape {
    RoomShape {
        size_m,
        walls,
        distance_m,
        centre_db,
        sub_db,
        angles,
        levels,
        distances: [distance_m; ROOM_SPEAKERS],
        mutes: MUTES,
        bass_management: true,
        crossover_hz: 80.0,
        music_upmix: false,
        upmix_amount: 0.6,
        renderer_version: 1,
        early_reflection_db: 0.0,
        ambience_mix: 0.0,
        ambience_decay_s: 0.5,
        ambience_damping_hz: 6000.0,
        preserve_position: false,
    }
}

/// A featured room: the same seven numbers, then the second renderer's sound.
/// `fill` is `None` when a stereo record stays on the front stage; `space` is
/// the walls' level in dB (-60 is none); `tail` is the ambience's share, its
/// length in seconds and the frequency above which it darkens.
const fn featured(base: RoomShape, fill: Option<f64>, space: f64, tail: [f64; 3]) -> RoomShape {
    RoomShape {
        size_m: base.size_m,
        walls: base.walls,
        distance_m: base.distance_m,
        centre_db: base.centre_db,
        sub_db: base.sub_db,
        angles: base.angles,
        levels: base.levels,
        distances: base.distances,
        mutes: base.mutes,
        bass_management: base.bass_management,
        crossover_hz: base.crossover_hz,
        music_upmix: fill.is_some(),
        upmix_amount: match fill {
            Some(amount) => amount,
            None => base.upmix_amount,
        },
        renderer_version: 2,
        early_reflection_db: space,
        ambience_mix: tail[0],
        ambience_decay_s: tail[1],
        ambience_damping_hz: tail[2],
        preserve_position: true,
    }
}

pub struct RoomSpec {
    pub index: i32,
    pub id: &'static str,
    pub label: &'static str,
    pub collection: RoomCollection,
    pub group: Option<RoomPresetGroup>,
    pub purpose: Option<&'static str>,
    pub shape: RoomShape,
}

/// Every built-in room, in wire order. `custom` (11) is not a preset.
pub const ROOM_CATALOG: &[RoomSpec] = &[
    RoomSpec {
        index: 0,
        id: "studio",
        label: "Studio",
        collection: RoomCollection::Classic,
        group: Some(RoomPresetGroup::Basic),
        purpose: None,
        shape: classic(3.2, 0.8, 1.4, 0.0, 0.0, RING, FLAT),
    },
    RoomSpec {
        index: 1,
        id: "livingRoom",
        label: "Living room",
        collection: RoomCollection::Classic,
        group: Some(RoomPresetGroup::Basic),
        purpose: None,
        shape: classic(4.2, 0.55, 1.8, 0.0, 0.0, RING, FLAT),
    },
    RoomSpec {
        index: 2,
        id: "cinema",
        label: "Cinema",
        collection: RoomCollection::Classic,
        group: Some(RoomPresetGroup::Playback),
        purpose: None,
        shape: classic(
            9.0,
            0.35,
            4.0,
            1.5,
            2.0,
            [-30.0, 30.0, 0.0, -105.0, 105.0, -145.0, 145.0],
            FLAT,
        ),
    },
    RoomSpec {
        index: 3,
        id: "frontStage",
        label: "Front stage",
        collection: RoomCollection::Classic,
        group: Some(RoomPresetGroup::Basic),
        purpose: None,
        shape: classic(
            4.2,
            0.6,
            2.0,
            0.0,
            0.0,
            RING,
            [0.0, 0.0, 0.0, -6.0, -6.0, -6.0, -6.0],
        ),
    },
    RoomSpec {
        index: 4,
        id: "nearField",
        label: "Near field",
        collection: RoomCollection::Classic,
        group: Some(RoomPresetGroup::Character),
        purpose: None,
        shape: classic(
            2.4,
            0.9,
            0.9,
            0.0,
            0.0,
            [-30.0, 30.0, 0.0, -90.0, 90.0, -135.0, 135.0],
            FLAT,
        ),
    },
    RoomSpec {
        index: 5,
        id: "homeTheatre",
        label: "Home theatre",
        collection: RoomCollection::Classic,
        group: Some(RoomPresetGroup::Playback),
        purpose: None,
        shape: classic(
            5.5,
            0.45,
            2.6,
            1.0,
            1.5,
            [-28.0, 28.0, 0.0, -110.0, 110.0, -150.0, 150.0],
            FLAT,
        ),
    },
    RoomSpec {
        index: 6,
        id: "gaming",
        label: "Gaming",
        collection: RoomCollection::Classic,
        group: Some(RoomPresetGroup::Playback),
        purpose: None,
        shape: classic(
            4.5,
            0.5,
            1.8,
            0.0,
            1.0,
            [-30.0, 30.0, 0.0, -95.0, 95.0, -135.0, 135.0],
            [0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0],
        ),
    },
    RoomSpec {
        index: 7,
        id: "concertHall",
        label: "Concert hall",
        collection: RoomCollection::Classic,
        group: Some(RoomPresetGroup::Playback),
        purpose: None,
        shape: classic(
            12.0,
            0.2,
            5.0,
            0.0,
            -2.0,
            [-25.0, 25.0, 0.0, -100.0, 100.0, -140.0, 140.0],
            [0.0, 0.0, 0.0, -3.0, -3.0, -4.0, -4.0],
        ),
    },
    RoomSpec {
        index: 8,
        id: "jazzClub",
        label: "Jazz club",
        collection: RoomCollection::Classic,
        group: Some(RoomPresetGroup::Playback),
        purpose: None,
        shape: classic(
            5.0,
            0.4,
            2.2,
            0.0,
            1.0,
            [-35.0, 35.0, 0.0, -100.0, 100.0, -140.0, 140.0],
            FLAT,
        ),
    },
    RoomSpec {
        index: 9,
        id: "club",
        label: "Club",
        collection: RoomCollection::Classic,
        group: Some(RoomPresetGroup::Playback),
        purpose: None,
        shape: classic(
            6.0,
            0.15,
            2.5,
            0.0,
            4.0,
            RING,
            [0.0, 0.0, 0.0, -1.0, -1.0, -2.0, -2.0],
        ),
    },
    RoomSpec {
        index: 10,
        id: "openAir",
        label: "Open air",
        collection: RoomCollection::Classic,
        group: Some(RoomPresetGroup::Character),
        purpose: None,
        shape: classic(12.0, 1.0, 3.0, 0.0, -1.0, RING, FLAT),
    },
    RoomSpec {
        index: 12,
        id: "referenceV2",
        label: "Reference",
        collection: RoomCollection::Featured,
        group: None,
        purpose: Some("A close, treated room. The record in front of you, and nothing added."),
        shape: featured(
            classic(3.6, 0.65, 1.2, 0.0, 0.0, RING, FLAT),
            None,
            -16.0,
            [0.0, 0.2, 8000.0],
        ),
    },
    RoomSpec {
        index: 13,
        id: "musicSpaceV2",
        label: "Music Space",
        collection: RoomCollection::Featured,
        group: None,
        purpose: Some("A wider stage with the voice held in front, and a little air after it."),
        shape: featured(
            classic(
                5.0,
                0.5,
                1.8,
                0.0,
                0.0,
                [-32.0, 32.0, 0.0, -100.0, 100.0, -140.0, 140.0],
                [0.0, 0.0, 0.0, -2.0, -2.0, -3.0, -3.0],
            ),
            Some(0.35),
            -12.0,
            [0.3, 0.35, 7000.0],
        ),
    },
    RoomSpec {
        index: 14,
        id: "cinemaV2",
        label: "Cinema",
        collection: RoomCollection::Featured,
        group: None,
        purpose: Some("A wide screen, a grounded voice, and the room around you."),
        shape: featured(
            classic(
                8.0,
                0.4,
                2.2,
                1.5,
                1.0,
                [-30.0, 30.0, 0.0, -105.0, 105.0, -145.0, 145.0],
                FLAT,
            ),
            Some(0.5),
            -8.0,
            [0.4, 0.7, 5000.0],
        ),
    },
    RoomSpec {
        index: 15,
        id: "gameWorldV2",
        label: "Game World",
        collection: RoomCollection::Featured,
        group: None,
        purpose: Some("A world around you, with short sounds left short."),
        shape: featured(
            classic(
                5.0,
                0.55,
                1.6,
                0.0,
                0.5,
                [-30.0, 30.0, 0.0, -95.0, 95.0, -140.0, 140.0],
                [0.0, 0.0, 0.0, 1.5, 1.5, 1.5, 1.5],
            ),
            None,
            -14.0,
            [0.3, 0.25, 8000.0],
        ),
    },
    RoomSpec {
        index: 16,
        id: "competitiveV2",
        label: "Competitive",
        collection: RoomCollection::Featured,
        group: None,
        purpose: Some("Direction and nothing else: no walls, no tail."),
        shape: featured(
            classic(
                3.0,
                0.85,
                1.0,
                0.0,
                0.0,
                [-30.0, 30.0, 0.0, -90.0, 90.0, -135.0, 135.0],
                FLAT,
            ),
            None,
            -60.0,
            [0.0, 0.1, 10000.0],
        ),
    },
    RoomSpec {
        index: 17,
        id: "liveVenueV2",
        label: "Live Venue",
        collection: RoomCollection::Featured,
        group: None,
        purpose: Some("A hall around the stage, with a long, soft tail."),
        shape: featured(
            classic(
                11.0,
                0.3,
                2.4,
                0.0,
                -1.0,
                [-28.0, 28.0, 0.0, -100.0, 100.0, -140.0, 140.0],
                [0.0, 0.0, 0.0, -2.0, -2.0, -3.0, -3.0],
            ),
            Some(0.55),
            -4.0,
            [0.6, 1.2, 4500.0],
        ),
    },
    RoomSpec {
        index: 18,
        id: "closeUpV2",
        label: "Close-up",
        collection: RoomCollection::Featured,
        group: None,
        purpose: Some(
            "A voice or a guitar an arm's length away, in a dead room. The player and nothing else.",
        ),
        shape: featured(
            classic(
                2.8,
                0.8,
                0.8,
                1.5,
                0.0,
                [-18.0, 18.0, 0.0, -90.0, 90.0, -135.0, 135.0],
                FLAT,
            ),
            None,
            -18.0,
            [0.0, 0.2, 8000.0],
        ),
    },
    RoomSpec {
        index: 19,
        id: "wideStageV2",
        label: "Wide Stage",
        collection: RoomCollection::Featured,
        group: None,
        purpose: Some("The pair set wide apart, for a record that should reach past your shoulders."),
        shape: featured(
            classic(
                5.5,
                0.5,
                2.0,
                1.0,
                0.0,
                [-45.0, 45.0, 0.0, -110.0, 110.0, -150.0, 150.0],
                [0.0, 0.0, 0.0, -2.0, -2.0, -4.0, -4.0],
            ),
            Some(0.25),
            -12.0,
            [0.3, 0.35, 7000.0],
        ),
    },
    RoomSpec {
        index: 20,
        id: "allAroundV2",
        label: "All Around",
        collection: RoomCollection::Featured,
        group: None,
        purpose: Some("A stereo record spread over every speaker: you sit in the middle of it."),
        shape: featured(
            classic(
                5.0,
                0.5,
                1.8,
                0.0,
                0.0,
                [-30.0, 30.0, 0.0, -90.0, 90.0, -150.0, 150.0],
                [0.0, 0.0, 0.0, -1.0, -1.0, -2.0, -2.0],
            ),
            Some(0.85),
            -12.0,
            [0.3, 0.35, 7000.0],
        ),
    },
    RoomSpec {
        index: 21,
        id: "balconyV2",
        label: "Balcony",
        collection: RoomCollection::Featured,
        group: None,
        purpose: Some(
            "Far from a narrow stage in a big hall: the band small in front, the hall all round.",
        ),
        shape: featured(
            classic(
                12.0,
                0.3,
                5.5,
                0.0,
                -1.0,
                [-18.0, 18.0, 0.0, -100.0, 100.0, -140.0, 140.0],
                [0.0, 0.0, 0.0, -3.0, -3.0, -4.0, -4.0],
            ),
            Some(0.5),
            -12.0,
            [0.5, 1.4, 4000.0],
        ),
    },
    RoomSpec {
        index: 22,
        id: "nightCinemaV2",
        label: "Night Cinema",
        collection: RoomCollection::Featured,
        group: None,
        purpose: Some(
            "Dialogue forward, the sub and the surrounds held down, for a film after everyone is asleep.",
        ),
        shape: featured(
            classic(
                6.0,
                0.55,
                2.2,
                3.0,
                -4.0,
                [-30.0, 30.0, 0.0, -105.0, 105.0, -145.0, 145.0],
                [0.0, 0.0, 0.0, -3.0, -3.0, -4.0, -4.0],
            ),
            Some(0.4),
            -12.0,
            [0.2, 0.5, 5000.0],
        ),
    },
    RoomSpec {
        index: 23,
        id: "conductorV2",
        label: "Conductor",
        collection: RoomCollection::Featured,
        group: None,
        purpose: Some("On the podium: the players wrap round you from far left to far right."),
        shape: featured(
            classic(
                8.0,
                0.35,
                2.5,
                2.0,
                0.0,
                [-60.0, 60.0, 0.0, -110.0, 110.0, -150.0, 150.0],
                [0.0, 0.0, 0.0, -1.0, -1.0, -3.0, -3.0],
            ),
            Some(0.8),
            -10.0,
            [0.4, 0.9, 5000.0],
        ),
    },
    RoomSpec {
        index: 24,
        id: "rearGuardV2",
        label: "Rear Guard",
        collection: RoomCollection::Featured,
        group: None,
        purpose: Some(
            "For hearing what is behind you: the surrounds and rears up, no walls, no tail.",
        ),
        shape: featured(
            classic(
                3.5,
                0.85,
                1.2,
                0.0,
                0.0,
                [-30.0, 30.0, 0.0, -90.0, 90.0, -140.0, 140.0],
                [0.0, 0.0, 0.0, 2.0, 2.0, 3.0, 3.0],
            ),
            None,
            -60.0,
            [0.0, 0.1, 10000.0],
        ),
    },
];

/// The picker's order: the featured rooms by purpose, then the classic rooms
/// by group. The wire order never moves; this one is free to.
const PICKER_ORDER: [i32; 24] = [
    12, 13, 19, 20, 18, 14, 22, 15, 16, 24, 17, 21, 23, 0, 1, 3, 5, 2, 6, 7, 8, 9, 4, 10,
];

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RoomListing {
    pub index: i32,
    pub id: String,
    pub label: String,
    pub collection: String,
    pub group: String,
    pub detail: String,
}

pub fn catalog() -> Vec<RoomListing> {
    PICKER_ORDER
        .iter()
        .filter_map(|index| spec(*index))
        .map(|spec| {
            let detail = match spec.purpose {
                Some(purpose) => purpose.to_string(),
                None => format!(
                    "{:.1} m · {:.0}% absorption",
                    spec.shape.size_m,
                    spec.shape.walls * 100.0
                ),
            };
            RoomListing {
                index: spec.index,
                id: spec.id.to_string(),
                label: spec.label.to_string(),
                collection: spec.collection.as_str().to_string(),
                group: spec
                    .group
                    .map(RoomPresetGroup::as_str)
                    .unwrap_or("")
                    .to_string(),
                detail,
            }
        })
        .collect()
}

pub fn spec(index: i32) -> Option<&'static RoomSpec> {
    ROOM_CATALOG.iter().find(|spec| spec.index == index)
}

/// The whole room `index` stands for, or nothing for `custom` and nonsense.
pub fn shape_of(index: i32) -> Option<RoomShape> {
    spec(index).map(|spec| spec.shape)
}

pub fn label_of(index: i32) -> Option<&'static str> {
    spec(index).map(|spec| spec.label)
}

/// Whether a room still holds every number its named preset stands for.
/// `enabled`, the head and the two source choices are the listener's and are
/// not compared.
fn has_shape(room: &FeqChainRoomSettings, shape: &RoomShape) -> bool {
    room.size_m == shape.size_m
        && room.walls == shape.walls
        && room.distance_m == shape.distance_m
        && room.centre_db == shape.centre_db
        && room.sub_db == shape.sub_db
        && room.angle_deg == shape.angles
        && room.level_db == shape.levels
        // The engine reads a speaker distance of zero as the ring's own, so a
        // room saved before per-speaker distances existed still matches.
        && (0..ROOM_SPEAKERS).all(|speaker| {
            let own = room.speaker_distance_m[speaker];
            let effective = if own > 0.0 { own } else { room.distance_m };
            effective == shape.distances[speaker]
        })
        && room.mute == shape.mutes
        && (room.bass_management != 0) == shape.bass_management
        && room.crossover_hz == shape.crossover_hz
        && (room.music_upmix != 0) == shape.music_upmix
        && room.upmix_amount == shape.upmix_amount
        && room.renderer_version == shape.renderer_version
        && room.early_reflection_db == shape.early_reflection_db
        && room.ambience_mix == shape.ambience_mix
        && room.ambience_decay_s == shape.ambience_decay_s
        && room.ambience_damping_hz == shape.ambience_damping_hz
        && (room.preserve_position != 0) == shape.preserve_position
}

/// Keeps a room's name only while the room still is that preset. A snapshot
/// written before the catalogue was ported names its room with the old
/// five-room numbers; those shapes no longer match, so the room becomes
/// `custom` instead of the picker claiming a room it is not.
pub fn normalize_preset(room: &mut FeqChainRoomSettings) {
    if room.preset == CUSTOM_PRESET {
        return;
    }
    match shape_of(room.preset) {
        Some(shape) if has_shape(room, &shape) => {}
        _ => room.preset = CUSTOM_PRESET,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalogue_has_the_24_built_in_rooms_and_no_duplicates() {
        assert_eq!(ROOM_CATALOG.len(), 24);
        let mut indices: Vec<i32> = ROOM_CATALOG.iter().map(|spec| spec.index).collect();
        indices.sort_unstable();
        indices.dedup();
        assert_eq!(indices.len(), 24);
        assert!(ROOM_CATALOG.iter().all(|spec| spec.index != 11));
        assert_eq!(catalog().len(), 24);
    }

    #[test]
    fn wire_numbers_keep_their_meaning() {
        assert_eq!(label_of(0), Some("Studio"));
        assert_eq!(label_of(1), Some("Living room"));
        assert_eq!(label_of(2), Some("Cinema"));
        assert_eq!(label_of(3), Some("Front stage"));
        assert_eq!(label_of(10), Some("Open air"));
        assert_eq!(shape_of(11), None);
        assert_eq!(label_of(12), Some("Reference"));
        assert_eq!(label_of(24), Some("Rear Guard"));

        let cinema = shape_of(2).expect("cinema");
        assert_eq!(cinema.size_m, 9.0);
        assert_eq!(cinema.distance_m, 4.0);
        assert_eq!(cinema.renderer_version, 1);
    }

    #[test]
    fn featured_rooms_turn_on_the_second_renderer_and_keep_their_positions() {
        let reference = shape_of(12).expect("reference");
        assert_eq!(reference.renderer_version, 2);
        assert!(!reference.music_upmix);
        assert_eq!(reference.early_reflection_db, -16.0);
        assert_eq!(reference.ambience_mix, 0.0);
        assert!(reference.preserve_position);

        let all_around = shape_of(20).expect("all around");
        assert!(all_around.music_upmix);
        assert_eq!(all_around.upmix_amount, 0.85);
        assert_eq!(all_around.levels[3], -1.0);
    }

    #[test]
    fn a_classic_room_stands_its_speakers_on_the_ring_at_its_distance() {
        let studio = shape_of(0).expect("studio");
        assert_eq!(studio.angles, RING);
        assert_eq!(studio.distances, [1.4; ROOM_SPEAKERS]);
        assert_eq!(studio.mutes, MUTES);
        assert!(studio.bass_management);
        assert_eq!(studio.crossover_hz, 80.0);
    }

    #[test]
    fn a_room_keeps_its_name_only_while_it_still_holds_that_shape() {
        let mut room = FeqChainRoomSettings::default();
        assert_eq!(room.preset, 1);
        normalize_preset(&mut room);
        assert_eq!(room.preset, 1, "the default living room still matches");

        let mut store = crate::AppStateStore::new(48_000.0);
        store.set_room_preset(12);
        let mut reference = store.chain_settings.room;
        normalize_preset(&mut reference);
        assert_eq!(reference.preset, 12, "an applied featured room matches");

        // The five-room build's Club: same number, another room.
        let mut old_club = FeqChainRoomSettings {
            preset: 4,
            size_m: 6.5,
            distance_m: 2.0,
            walls: 0.5,
            ambience_mix: 0.3,
            ambience_decay_s: 0.65,
            ..Default::default()
        };
        normalize_preset(&mut old_club);
        assert_eq!(old_club.preset, CUSTOM_PRESET);
    }
}
