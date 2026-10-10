//! Port of `slots.c`, `internal/slots.h`: paired physical voice slots and the active/sustained note bitmaps.
//!
//! Pair `i` drives MIDI channel `i` below 9 and `i + 1` above, since channel 9 is the drums; the native AdLib
//! driver uses all 32 logical parts directly. Each pair has two halves so a share-parts handoff can bind two
//! players' channels to one physical channel. Half 0 uses the P1 bitmaps and half 1 the P2 bitmaps.

use super::Imuse;
use super::players::ChannelRef;

const CC_MODULATION: u32 = 1;
const CC_VOLUME: u32 = 7;
const CC_PAN: u32 = 10;
const CC_PART_NOTEREQ: u32 = 17;
const CC_PART_PRIORITY: u32 = 18;

/// MIDI channel of the drums, outside the pair pool.
pub(crate) const DRUM_CHANNEL: i32 = 9;

/// Voice pairs in the pool; the standard backends use the first 15.
pub(crate) const PAIR_COUNT: usize = 32;

/// A physical voice slot: one half of a pair.
#[doc(alias = "ImPart")]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Part {
    /// Owning player.
    pub player: Option<u8>,
    /// Owning channel.
    pub out_channel: Option<ChannelRef>,
    /// MIDI channel 0..=15, or AdLib part 0..=31.
    pub out_channel_id: i32,
    pub pgm: i32,
    pub priority: i32,
    pub note_req: i32,
    pub volume: i32,
    pub pan: i32,
    pub modulation: i32,
    /// Engine-internal signed bend.
    pub pitch_bend: i32,
    pub sustain: i32,
}

/// A part, named by its pair and half. The sibling is the other half of the same pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PartRef {
    pub pair: u8,
    pub half: u8,
}

impl PartRef {
    /// `sharedMidiChannel`: the other half of the pair.
    pub fn shared(self) -> Self {
        Self {
            pair: self.pair,
            half: self.half ^ 1,
        }
    }
}

#[doc(alias = "ImSlotsState")]
#[derive(Debug, Clone)]
pub(crate) struct SlotsState {
    /// `[half][note]`: bit `outChannelId` set while the note sounds.
    pub active_notes: [[u32; 128]; 2],
    /// `[half][note]`: notes held past release by the sustain pedal.
    pub sustained_notes: [[u32; 128]; 2],
    /// `[pair][half]`.
    pub parts: [[Part; 2]; PAIR_COUNT],
    pub drum_priority: i32,
    pub drum_part_note_req: i32,
    pub drum_volume: i32,
    /// Suppresses driver notes during silent replays.
    pub drv_ignore_notes: bool,
}

impl Default for SlotsState {
    fn default() -> Self {
        Self {
            active_notes: [[0; 128]; 2],
            sustained_notes: [[0; 128]; 2],
            parts: [[Part::default(); 2]; PAIR_COUNT],
            drum_priority: 0,
            drum_part_note_req: 0,
            drum_volume: 0,
            drv_ignore_notes: false,
        }
    }
}

fn channel_bit(out_channel_id: i32) -> u32 {
    1 << out_channel_id
}

impl Imuse {
    pub(crate) fn part(&self, part: PartRef) -> &Part {
        &self.slots.parts[usize::from(part.pair)][usize::from(part.half)]
    }

    pub(crate) fn part_mut(&mut self, part: PartRef) -> &mut Part {
        &mut self.slots.parts[usize::from(part.pair)][usize::from(part.half)]
    }

    fn native_adlib(&self) -> bool {
        self.midi.backend.is_native_adlib()
    }

    fn part_count(&self) -> usize {
        self.midi.backend.melodic_channels()
    }

    /// Whether the player bound to a part plays a Roland (`ROL `) sound.
    fn part_is_roland(&self, part: PartRef) -> bool {
        self.part(part)
            .player
            .is_some_and(|player| self.seq.data[usize::from(player)].dialect == 3)
    }

    /// Resets the pairs and pushes their initial controller state, then the drum channel's.
    #[doc(alias("ImSlots_Init", "ImSlots_Deinit"))]
    pub(crate) fn slots_init(&mut self) {
        tracing::info!(target: "imuse", "SLOTS module...");
        self.slots.drv_ignore_notes = false;
        self.slots.active_notes = [[0; 128]; 2];
        self.slots.sustained_notes = [[0; 128]; 2];
        let native = self.native_adlib();
        for i in 0..self.part_count() {
            let out_channel_id = if native || i < 9 {
                i as i32
            } else {
                i as i32 + 1
            };
            let fresh = Part {
                out_channel_id,
                volume: 127,
                pan: 64,
                ..Part::default()
            };
            self.slots.parts[i] = [fresh; 2];
            let part = fresh;
            self.drv_program_change(out_channel_id, part.pgm);
            self.drv_control_change(out_channel_id, CC_PART_PRIORITY, part.priority);
            self.drv_control_change(out_channel_id, CC_PART_NOTEREQ, part.note_req);
            self.drv_control_change(out_channel_id, CC_VOLUME, part.volume);
            self.drv_control_change(out_channel_id, CC_PAN, part.pan);
            self.drv_control_change(out_channel_id, CC_MODULATION, part.modulation);
            self.drv_set_pitch_bend(out_channel_id, 2 * part.pitch_bend + 0x2000);
        }
        if native {
            return;
        }
        self.slots.drum_priority = 0;
        self.drv_control_change(DRUM_CHANNEL, CC_PART_PRIORITY, 0);
        self.slots.drum_part_note_req = 1;
        self.drv_control_change(DRUM_CHANNEL, CC_PART_NOTEREQ, 1);
        self.slots.drum_volume = 127;
        self.drv_control_change(DRUM_CHANNEL, CC_VOLUME, 127);
    }

    /// The first pair whose halves are both unbound.
    #[doc(alias = "ImSlots_GetFreePlayerParts")]
    pub(crate) fn slots_get_free_player_parts(&self) -> Option<u8> {
        (0..self.part_count())
            .find(|&i| {
                self.slots.parts[i][0].player.is_none() && self.slots.parts[i][1].player.is_none()
            })
            .map(|i| i as u8)
    }

    #[doc(alias = "ImSlots_HandleNoteOn")]
    pub(crate) fn slots_handle_note_on(&mut self, part: Option<PartRef>, note: i32, velocity: i32) {
        let Some(part) = part else {
            return;
        };
        let out = self.part(part).out_channel_id;
        let bit = channel_bit(out);
        let half = usize::from(part.half);
        let note_index = note as usize;
        if self.native_adlib() {
            // DOS93 allocates every NoteOn, including repeated or pedal-held notes.
            self.slots.active_notes[half][note_index] |= bit;
            self.drv_note_on(out, note, velocity);
            return;
        }
        if self.part_is_roland(part) {
            // DOS93 Roland sends a repeated NoteOn without a preceding NoteOff.
            self.slots.active_notes[half][note_index] |= bit;
            self.slots.sustained_notes[half][note_index] &= !bit;
            self.drv_note_on(out, note, velocity);
            return;
        }
        if self.slots.active_notes[half][note_index] & bit != 0 {
            // Retrigger of a sounding note restarts its envelope.
            self.drv_note_off(out, note);
            self.drv_note_on(out, note, velocity);
            return;
        }
        if self.slots.sustained_notes[half][note_index] & bit != 0 {
            // Retrigger of a pedal-held note promotes it back to sounding.
            self.slots.sustained_notes[half][note_index] &= !bit;
            self.slots.active_notes[half][note_index] |= bit;
            self.drv_note_off(out, note);
            self.drv_note_on(out, note, velocity);
            return;
        }
        self.slots.active_notes[half][note_index] |= bit;
        self.drv_note_on(out, note, velocity);
    }

    /// Releases a note, or parks it in the sustained bank while the pedal is down.
    #[doc(alias = "ImSlots_HandleNoteOff")]
    pub(crate) fn slots_handle_note_off(&mut self, part: Option<PartRef>, note: i32) {
        let Some(part) = part else {
            return;
        };
        let Part {
            out_channel_id: out,
            sustain,
            ..
        } = *self.part(part);
        let bit = channel_bit(out);
        let half = usize::from(part.half);
        let note_index = note as usize;
        if self.native_adlib() {
            self.slots.active_notes[half][note_index] &= !bit;
            if sustain != 0 {
                self.slots.sustained_notes[half][note_index] |= bit;
            } else {
                self.slots.sustained_notes[half][note_index] &= !bit;
            }
            self.drv_note_off(out, note);
            return;
        }
        if self.part_is_roland(part) {
            self.slots.active_notes[half][note_index] &= !bit;
            self.slots.sustained_notes[half][note_index] &= !bit;
            self.drv_note_off(out, note);
            return;
        }
        if self.slots.active_notes[half][note_index] & bit == 0 {
            return;
        }
        self.slots.active_notes[half][note_index] &= !bit;
        if sustain != 0 {
            self.slots.sustained_notes[half][note_index] |= bit;
        } else {
            self.drv_note_off(out, note);
        }
    }

    /// A drum note on channel 9, pushing the cached drum controllers only when they change.
    #[doc(alias = "ImSlots_HandleDrumNoteOn")]
    pub(crate) fn slots_handle_drum_note_on(
        &mut self,
        priority: i32,
        note_req: i32,
        volume: i32,
        note: i32,
        velocity: i32,
    ) {
        if priority != self.slots.drum_priority {
            self.slots.drum_priority = priority;
            self.drv_control_change(DRUM_CHANNEL, CC_PART_PRIORITY, priority);
        }
        if note_req != self.slots.drum_part_note_req {
            self.slots.drum_part_note_req = note_req;
            self.drv_control_change(DRUM_CHANNEL, CC_PART_NOTEREQ, note_req);
        }
        if volume != self.slots.drum_volume {
            self.slots.drum_volume = volume;
            self.drv_control_change(DRUM_CHANNEL, CC_VOLUME, volume);
        }
        self.drv_note_on(DRUM_CHANNEL, note, velocity);
    }

    #[doc(alias = "ImSlots_NoteOffDrumChannel")]
    pub(crate) fn slots_note_off_drum_channel(&mut self, note: i32) {
        self.drv_note_off(DRUM_CHANNEL, note);
    }

    /// Silences every note tracked on a part and releases its pedal. The back-links stay.
    #[doc(alias = "ImSlots_FreeMidiChannel")]
    pub(crate) fn slots_free_midi_channel(&mut self, part: Option<PartRef>) {
        let Some(part) = part else {
            return;
        };
        let out = self.part(part).out_channel_id;
        let bit = channel_bit(out);
        let half = usize::from(part.half);
        self.part_mut(part).sustain = 0;
        if self.native_adlib() {
            if !self.slots.drv_ignore_notes {
                self.drv_control_change(out, 123, 0);
            }
            for note in 0..128 {
                self.slots.active_notes[half][note] &= !bit;
                self.slots.sustained_notes[half][note] &= !bit;
            }
            return;
        }
        for note in 0..128 {
            if self.slots.active_notes[half][note] & bit != 0 {
                self.drv_note_off(out, note as i32);
                self.slots.active_notes[half][note] &= !bit;
            } else if self.slots.sustained_notes[half][note] & bit != 0 {
                self.drv_note_off(out, note as i32);
                self.slots.sustained_notes[half][note] &= !bit;
            }
        }
    }

    /// Stores the pedal state; releasing it ends every note it held.
    #[doc(alias = "ImSlots_SetChannelSustain")]
    pub(crate) fn slots_set_channel_sustain(&mut self, part: Option<PartRef>, sustain: i32) {
        let Some(part) = part else {
            return;
        };
        self.part_mut(part).sustain = sustain;
        let out = self.part(part).out_channel_id;
        let bit = channel_bit(out);
        let half = usize::from(part.half);
        if self.native_adlib() {
            self.drv_control_change(out, 64, sustain);
            if sustain == 0 {
                for note in 0..128 {
                    self.slots.sustained_notes[half][note] &= !bit;
                }
            }
            return;
        }
        if sustain != 0 {
            return;
        }
        for note in 0..128 {
            if self.slots.sustained_notes[half][note] & bit != 0 {
                self.slots.sustained_notes[half][note] &= !bit;
                self.drv_note_off(out, note as i32);
            }
        }
    }

    /// Snapshots the notes sounding on the parts of `player` (the sustained bank excluded) and returns how many
    /// there are.
    #[doc(alias("ImSlots_GetPlayerNotes", "popcount_u32"))]
    pub(crate) fn slots_get_player_notes(&self, player: u8, notes_mask: &mut [u32; 128]) -> i32 {
        let mut count = 0;
        let mut p1_mask = 0;
        for i in 0..self.part_count() {
            if self.slots.parts[i][0].player == Some(player) {
                p1_mask |= channel_bit(self.slots.parts[i][0].out_channel_id);
            }
        }
        for (n, mask) in notes_mask.iter_mut().enumerate() {
            let bits = p1_mask & self.slots.active_notes[0][n];
            *mask = bits;
            count += bits.count_ones() as i32;
        }
        let mut p2_mask = 0;
        for i in 0..self.part_count() {
            if self.slots.parts[i][1].player == Some(player) {
                p2_mask |= channel_bit(self.slots.parts[i][1].out_channel_id);
            }
        }
        if self.native_adlib() && !self.slots.drv_ignore_notes {
            // Query the sequencer-owned voices, not stale NoteOn bits after stealing.
            self.midi.backend.adlib_notes(p1_mask | p2_mask, notes_mask);
            return notes_mask.iter().map(|bits| bits.count_ones() as i32).sum();
        }
        for (n, mask) in notes_mask.iter_mut().enumerate() {
            let bits = p2_mask & self.slots.active_notes[1][n];
            *mask |= bits;
            count += bits.count_ones() as i32;
        }
        count
    }

    /// Re-sends a NoteOn for every note tracked on the parts of `player`, after a silent replay.
    #[doc(alias = "ImSlots_RestartPlayerNotes")]
    pub(crate) fn slots_restart_player_notes(&mut self, player: u8) {
        let native = self.native_adlib();
        // Scan bitmaps do not retain velocity: DOS93 uses 80, TIE 64.
        let velocity = if native { 80 } else { 64 };
        for note in 0..128 {
            for half in 0..2 {
                let mut bits =
                    self.slots.sustained_notes[half][note] | self.slots.active_notes[half][note];
                let mut pair = 0;
                while bits != 0 {
                    if bits & 1 != 0 && self.slots.parts[pair][half].player == Some(player) {
                        let out = self.slots.parts[pair][half].out_channel_id;
                        self.drv_note_on(out, note as i32, velocity);
                    }
                    bits >>= 1;
                    pair += 1;
                    // Skip the drum channel's position in the mask.
                    if pair == 9 && !native {
                        bits >>= 1;
                    }
                }
            }
        }
    }

    #[doc(alias = "ImSlots_SetDrvIgnoreNotes")]
    pub(crate) fn slots_set_drv_ignore_notes(&mut self) {
        self.slots.drv_ignore_notes = true;
    }

    #[doc(alias = "ImSlots_ClearDrvIgnoreNotes")]
    pub(crate) fn slots_clear_drv_ignore_notes(&mut self) {
        self.slots.drv_ignore_notes = false;
    }

    /// Mirrors a changed part setting to the sibling half and returns whether it changed.
    fn part_set(&mut self, part: PartRef, value: i32, field: fn(&mut Part) -> &mut i32) -> bool {
        if *field(self.part_mut(part)) == value {
            return false;
        }
        *field(self.part_mut(part.shared())) = value;
        *field(self.part_mut(part)) = value;
        true
    }

    #[doc(alias = "ImSlots_PartSetPgm")]
    pub(crate) fn slots_part_set_pgm(&mut self, part: Option<PartRef>, pgm: i32) {
        if let Some(part) = part
            && self.part_set(part, pgm, |p| &mut p.pgm)
        {
            let out = self.part(part).out_channel_id;
            self.drv_program_change(out, pgm);
        }
    }

    #[doc(alias = "ImSlots_PartSetPriority")]
    pub(crate) fn slots_part_set_priority(&mut self, part: Option<PartRef>, priority: i32) {
        if let Some(part) = part
            && self.part_set(part, priority, |p| &mut p.priority)
        {
            let out = self.part(part).out_channel_id;
            self.drv_control_change(out, CC_PART_PRIORITY, priority);
        }
    }

    #[doc(alias = "ImSlots_PartSetNoteReq")]
    pub(crate) fn slots_part_set_note_req(&mut self, part: Option<PartRef>, note_req: i32) {
        if let Some(part) = part
            && self.part_set(part, note_req, |p| &mut p.note_req)
        {
            let out = self.part(part).out_channel_id;
            self.drv_control_change(out, CC_PART_NOTEREQ, note_req);
        }
    }

    #[doc(alias = "ImSlots_PartSetVolume")]
    pub(crate) fn slots_part_set_volume(&mut self, part: Option<PartRef>, volume: i32) {
        if let Some(part) = part
            && self.part_set(part, volume, |p| &mut p.volume)
        {
            let out = self.part(part).out_channel_id;
            self.drv_control_change(out, CC_VOLUME, volume);
        }
    }

    #[doc(alias = "ImSlots_PartSetPan")]
    pub(crate) fn slots_part_set_pan(&mut self, part: Option<PartRef>, pan: i32) {
        if let Some(part) = part
            && self.part_set(part, pan, |p| &mut p.pan)
        {
            let out = self.part(part).out_channel_id;
            self.drv_control_change(out, CC_PAN, pan);
        }
    }

    #[doc(alias = "ImSlots_PartSetModulation")]
    pub(crate) fn slots_part_set_modulation(&mut self, part: Option<PartRef>, modulation: i32) {
        if let Some(part) = part
            && self.part_set(part, modulation, |p| &mut p.modulation)
        {
            let out = self.part(part).out_channel_id;
            self.drv_control_change(out, CC_MODULATION, modulation);
        }
    }

    /// `pitch_bend` is the engine's signed value; standard drivers get the 14-bit wire form `2 * value + 0x2000`.
    #[doc(alias = "ImSlots_PartSetPitchBend")]
    pub(crate) fn slots_part_set_pitch_bend(&mut self, part: Option<PartRef>, pitch_bend: i32) {
        if let Some(part) = part
            && self.part_set(part, pitch_bend, |p| &mut p.pitch_bend)
        {
            let out = self.part(part).out_channel_id;
            if self.native_adlib() {
                self.midi.backend.adlib_bend(out, pitch_bend);
            } else {
                self.drv_set_pitch_bend(out, 2 * pitch_bend + 0x2000);
            }
        }
    }

    #[doc(alias = "ImSlots_DrvProgramChange")]
    pub(crate) fn drv_program_change(&mut self, out_channel: i32, pgm: i32) {
        self.midi.backend.program_change(out_channel, pgm);
    }

    #[doc(alias = "ImSlots_DrvNoteOn")]
    pub(crate) fn drv_note_on(&mut self, out_channel: i32, note: i32, velocity: i32) {
        if !self.slots.drv_ignore_notes {
            self.midi.backend.note_on(out_channel, note, velocity);
        }
    }

    #[doc(alias = "ImSlots_DrvNoteOff")]
    pub(crate) fn drv_note_off(&mut self, out_channel: i32, note: i32) {
        if !self.slots.drv_ignore_notes {
            self.midi.backend.note_off(out_channel, note);
        }
    }

    #[doc(alias = "ImSlots_DrvControlChange")]
    pub(crate) fn drv_control_change(&mut self, out_channel: i32, controller: u32, value: i32) {
        self.midi
            .backend
            .control_change(out_channel, controller, value);
    }

    #[doc(alias = "ImSlots_DrvSetPitchBend")]
    pub(crate) fn drv_set_pitch_bend(&mut self, out_channel: i32, wire: i32) {
        self.midi.backend.pitch_bend(out_channel, wire);
    }
}
