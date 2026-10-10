//! Port of `fm4_driver.c`, `internal/fm4_driver.h`: the TIE FM4 driver, recovered from FM4.MDR, driving a
//! two-bank OPL3 with nine voices.

use super::fm4_data::{
    ATTENUATION, CARRIER_OFFSETS, CHANNEL_COUNT, DEFAULT_TONES, F_NUMBERS, FNUMBER_TABLE_COUNT,
    MODULATOR_OFFSETS, NOTE_FNUMBER_ROWS, NOTE_OCTAVES, NOTE_TABLE_COUNT, PERCUSSION_PITCH,
    PERCUSSION_TONES, TONE_COUNT, TONE_FIELD_COUNT, TONE_FIELDS, ToneRecord, VOICE_COUNT,
};

/// Receives OPL register writes; `reg` carries the bank in bit 8.
#[doc(alias = "ImFm4WriteRegFunc")]
pub(crate) trait OplWriter {
    fn write(&mut self, reg: u16, value: u8);
}

#[doc(alias = "IM_FM4_INVALID_VOICE")]
const INVALID_VOICE: u8 = u8::MAX;
const BANK_0: usize = 0;
const BANK_1: usize = 1;
const CONTROLLER_VOLUME: u32 = 7;
const CONTROLLER_PAN: u32 = 10;
const CONTROLLER_NOTE_REQUEST: u32 = 17;
const CONTROLLER_PRIORITY: u32 = 18;
const PERCUSSION_CHANNEL: i32 = 9;

/// Fields of a packed tone record.
#[doc(alias = "ImFm4ToneField")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToneField {
    CarrierLevel = 0,
    CarrierVelocitySensitivity = 1,
    ModulatorLevel = 13,
    ModulatorVelocitySensitivity = 14,
    Connection = 26,
    /// Not a packed field: the record's duration byte.
    Duration = 68,
}

#[doc(alias = "ImFm4ChannelState")]
#[derive(Debug, Clone, Copy, Default)]
struct ChannelState {
    priority: i32,
    note_request: i32,
    voice_count: i32,
    overflow: bool,
    program: i32,
    volume: i32,
    pan: i32,
    pitch_bend: i32,
}

#[doc(alias = "ImFm4VoiceState")]
#[derive(Debug, Clone, Copy, Default)]
struct VoiceState {
    algorithm: [i32; 2],
    carrier_base: [i32; 2],
    modulator_base: [i32; 2],
}

#[doc(alias = "ImFm4VoiceNode")]
#[derive(Debug, Clone, Copy, Default)]
struct VoiceNode {
    prev: u8,
    next: u8,
    midi_channel: u8,
    note: u8,
}

#[doc(alias = "ImFm4BankLevels")]
#[derive(Debug, Clone, Copy)]
struct BankLevels {
    carrier_base: i32,
    modulator_base: i32,
    carrier: i32,
    modulator: i32,
    algorithm: i32,
}

#[doc(alias = "ImFm4Driver")]
#[derive(Debug, Clone)]
pub(crate) struct Fm4Driver {
    channels: [ChannelState; CHANNEL_COUNT],
    voices: [VoiceState; VOICE_COUNT],
    voice_nodes: [VoiceNode; VOICE_COUNT],
    free_head: u8,
    active_head: u8,
    pub tones: [[ToneRecord; TONE_COUNT]; 2],
    registers: [[u8; 256]; 2],
}

fn valid_channel(channel: i32) -> bool {
    (0..CHANNEL_COUNT as i32).contains(&channel)
}

/// Arithmetic right shift rounding towards minus infinity.
fn shift_right_signed(value: i32, shift: u32) -> i32 {
    if value >= 0 {
        value >> shift
    } else {
        -((((-value) as u32 + ((1 << shift) - 1)) >> shift) as i32)
    }
}

impl Fm4Driver {
    /// Resets the driver and the chip's registers (`FM4_OpInit`, 0x10005).
    #[doc(alias = "im_fm4_driver_init")]
    pub fn new(out: &mut dyn OplWriter) -> Self {
        let mut driver = Self {
            channels: [ChannelState::default(); CHANNEL_COUNT],
            voices: [VoiceState::default(); VOICE_COUNT],
            voice_nodes: [VoiceNode::default(); VOICE_COUNT],
            free_head: INVALID_VOICE,
            active_head: INVALID_VOICE,
            tones: DEFAULT_TONES,
            registers: [[0; 256]; 2],
        };
        driver.init_hardware(out);
        for channel in &mut driver.channels {
            channel.note_request = 1;
            channel.volume = 127;
            channel.pan = 64;
            channel.pitch_bend = 0x2000;
        }
        for voice in 0..VOICE_COUNT {
            driver.voice_nodes[voice] = VoiceNode {
                prev: INVALID_VOICE,
                next: INVALID_VOICE,
                ..VoiceNode::default()
            };
            let mut head = driver.free_head;
            driver.list_add(&mut head, voice as u8);
            driver.free_head = head;
        }
        driver
    }

    fn list_add(&mut self, head: &mut u8, index: u8) {
        let node = &mut self.voice_nodes[usize::from(index)];
        node.prev = INVALID_VOICE;
        node.next = *head;
        if *head != INVALID_VOICE {
            self.voice_nodes[usize::from(*head)].prev = index;
        }
        *head = index;
    }

    fn list_remove(&mut self, head: &mut u8, index: u8) {
        let VoiceNode { prev, next, .. } = self.voice_nodes[usize::from(index)];
        if next != INVALID_VOICE {
            self.voice_nodes[usize::from(next)].prev = prev;
        }
        if prev != INVALID_VOICE {
            self.voice_nodes[usize::from(prev)].next = next;
        } else {
            *head = next;
        }
        let node = &mut self.voice_nodes[usize::from(index)];
        node.prev = INVALID_VOICE;
        node.next = INVALID_VOICE;
    }

    fn list_tail(&self, head: u8) -> u8 {
        let mut index = head;
        while index != INVALID_VOICE && self.voice_nodes[usize::from(index)].next != INVALID_VOICE {
            index = self.voice_nodes[usize::from(index)].next;
        }
        index
    }

    fn write_raw(&mut self, out: &mut dyn OplWriter, bank: usize, reg: u32, value: u32) {
        let byte = value as u8;
        self.registers[bank][(reg & 0xff) as usize] = byte;
        out.write((bank * 0x100) as u16 + (reg & 0xff) as u16, byte);
    }

    fn write_cached(&mut self, out: &mut dyn OplWriter, bank: usize, reg: u32, value: u32) {
        let byte = value as u8;
        let reg = reg & 0xff;
        if self.registers[bank][reg as usize] != byte {
            self.write_raw(out, bank, reg, u32::from(byte));
        }
    }

    fn get_tone(&self, bank: usize, tone: i32) -> ToneRecord {
        if (0..TONE_COUNT as i32).contains(&tone) {
            self.tones[bank][tone as usize]
        } else {
            ToneRecord::default()
        }
    }

    fn get_tone_field(&self, bank: usize, tone: i32, field: ToneField) -> u32 {
        if !(0..TONE_COUNT as i32).contains(&tone) {
            return 0;
        }
        let record = &self.tones[bank][tone as usize];
        let field = field as usize;
        if field < TONE_FIELD_COUNT {
            let desc = TONE_FIELDS[field];
            return u32::from((record.packed[desc.byte_offset] & desc.mask) >> desc.shift);
        }
        if field == ToneField::Duration as usize {
            return u32::from(record.duration);
        }
        0
    }

    fn clear_hardware(&mut self, out: &mut dyn OplWriter) {
        for reg in 0..256 {
            self.write_raw(out, BANK_0, reg, 0);
            self.write_raw(out, BANK_1, reg, 0);
        }
    }

    /// `FM4_TonesInitHardware`, 0x108fe.
    fn init_hardware(&mut self, out: &mut dyn OplWriter) {
        self.clear_hardware(out);
        self.write_cached(out, BANK_0, 0x08, 0x40);
        self.write_cached(out, BANK_0, 0xbd, 0x00);
        self.write_cached(out, BANK_1, 0x05, 0x01);
    }

    fn program_bank_instrument(
        &mut self,
        out: &mut dyn OplWriter,
        bank: usize,
        voice: usize,
        tone: &ToneRecord,
    ) {
        let modulator = u32::from(MODULATOR_OFFSETS[voice]);
        let carrier = u32::from(CARRIER_OFFSETS[voice]);
        let route = if bank == BANK_0 { 0x20 } else { 0x10 };
        let p = &tone.packed;
        self.write_cached(out, bank, modulator + 0x40, 0x3f);
        self.write_cached(out, bank, carrier + 0x40, 0x3f);
        self.write_cached(out, bank, modulator + 0x20, u32::from(p[0]));
        self.write_cached(out, bank, modulator + 0x40, u32::from(p[1] | 0x3f));
        self.write_cached(out, bank, modulator + 0x60, u32::from(!p[2]));
        self.write_cached(out, bank, modulator + 0x80, u32::from(!p[3]));
        self.write_cached(out, bank, modulator + 0xe0, u32::from(p[4]));
        self.write_cached(out, bank, carrier + 0x20, u32::from(p[5]));
        self.write_cached(out, bank, carrier + 0x40, u32::from(p[6] | 0x3f));
        self.write_cached(out, bank, carrier + 0x60, u32::from(!p[7]));
        self.write_cached(out, bank, carrier + 0x80, u32::from(!p[8]));
        self.write_cached(out, bank, carrier + 0xe0, u32::from(p[9]));
        self.write_cached(out, bank, voice as u32 + 0xc0, u32::from(p[10]) | route);
    }

    fn set_operator_level(
        &mut self,
        out: &mut dyn OplWriter,
        bank: usize,
        voice: usize,
        field: ToneField,
        att: i32,
    ) {
        let offset = match field {
            ToneField::CarrierLevel => CARRIER_OFFSETS[voice],
            ToneField::ModulatorLevel => MODULATOR_OFFSETS[voice],
            _ => return,
        };
        let reg = u32::from(offset) + 0x40;
        let value = (i32::from(self.registers[bank][reg as usize]) | 0x3f) - att;
        self.write_cached(out, bank, reg, value as u32);
    }

    /// `FM4_TonesProgramPitch`, 0x10cba.
    fn program_pitch(&mut self, out: &mut dyn OplWriter, voice: usize, note: i32, bend_delta: i32) {
        let note_index = (shift_right_signed(bend_delta, 8) + note - 7)
            .clamp(0, NOTE_TABLE_COUNT as i32 - 1) as usize;
        let fine = shift_right_signed(bend_delta, 4) as u32 & 0x0f;
        let f_index =
            (u32::from(NOTE_FNUMBER_ROWS[note_index]) + fine).min(FNUMBER_TABLE_COUNT as u32 - 1);
        let f_number = F_NUMBERS[f_index as usize];
        let b0 = (f_number >> 8)
            + (u32::from(NOTE_OCTAVES[note_index]) << 2)
            + (u32::from(self.registers[BANK_0][voice + 0xb0]) & 0x20);
        self.write_cached(out, BANK_0, voice as u32 + 0xa0, f_number);
        self.write_cached(out, BANK_0, voice as u32 + 0xb0, b0);
        self.write_cached(out, BANK_1, voice as u32 + 0xa0, f_number);
        self.write_cached(out, BANK_1, voice as u32 + 0xb0, b0);
    }

    fn key_off_voice(&mut self, out: &mut dyn OplWriter, voice: usize) {
        let reg = voice as u32 + 0xb0;
        let bank0 = u32::from(self.registers[BANK_0][reg as usize]) & 0xdf;
        self.write_cached(out, BANK_0, reg, bank0);
        let bank1 = u32::from(self.registers[BANK_1][reg as usize]) & 0xdf;
        self.write_cached(out, BANK_1, reg, bank1);
    }

    fn calculate_bank_levels(
        &self,
        bank: usize,
        tone: i32,
        velocity_attenuation: i32,
        volume: i32,
    ) -> BankLevels {
        let field = |field| self.get_tone_field(bank, tone, field) as i32;
        let algorithm = field(ToneField::Connection);
        let carrier_velocity =
            ((field(ToneField::CarrierVelocitySensitivity) + 1) * velocity_attenuation) >> 6;
        let carrier_base = (carrier_velocity + field(ToneField::CarrierLevel)).clamp(0, 63);
        let modulator_velocity =
            ((field(ToneField::ModulatorVelocitySensitivity) + 1) * velocity_attenuation) >> 6;
        let modulator_base = (modulator_velocity + field(ToneField::ModulatorLevel)).clamp(0, 63);
        let carrier = ATTENUATION[((volume * (carrier_base + 1)) >> 7) as usize];
        let modulator = if algorithm == 1 {
            ATTENUATION[((volume * (modulator_base + 1)) >> 7) as usize]
        } else {
            modulator_base
        };
        BankLevels {
            carrier_base,
            modulator_base,
            carrier,
            modulator,
            algorithm,
        }
    }

    /// `FM4_TonesStartVoice`, 0x1040f.
    #[allow(clippy::too_many_arguments)]
    fn start_voice(
        &mut self,
        out: &mut dyn OplWriter,
        voice: usize,
        note: i32,
        velocity: i32,
        tone: i32,
        volume: i32,
        pitch_bend: i32,
    ) {
        let velocity_attenuation = 2 * ATTENUATION[(velocity as u32 >> 1) as usize];
        let bank0 = self.calculate_bank_levels(BANK_0, tone, velocity_attenuation, volume);
        let bank1 = self.calculate_bank_levels(BANK_1, tone, velocity_attenuation, volume);
        // The note, channel and duration upstream also stores are read only by the unscheduled
        // `im_fm4_driver_tick_voice`.
        self.voices[voice] = VoiceState {
            algorithm: [bank0.algorithm, bank1.algorithm],
            carrier_base: [bank0.carrier_base, bank1.carrier_base],
            modulator_base: [bank0.modulator_base, bank1.modulator_base],
        };
        let tone0 = self.get_tone(BANK_0, tone);
        let tone1 = self.get_tone(BANK_1, tone);
        self.program_bank_instrument(out, BANK_0, voice, &tone0);
        self.program_bank_instrument(out, BANK_1, voice, &tone1);
        self.program_pitch(out, voice, note, shift_right_signed(pitch_bend - 0x2000, 1));
        if !(0..TONE_COUNT as i32).contains(&tone) {
            return;
        }
        self.set_operator_level(
            out,
            BANK_0,
            voice,
            ToneField::ModulatorLevel,
            bank0.modulator,
        );
        self.set_operator_level(out, BANK_0, voice, ToneField::CarrierLevel, bank0.carrier);
        self.set_operator_level(
            out,
            BANK_1,
            voice,
            ToneField::ModulatorLevel,
            bank1.modulator,
        );
        self.set_operator_level(out, BANK_1, voice, ToneField::CarrierLevel, bank1.carrier);
        let reg = voice + 0xb0;
        let bank0_key = u32::from(self.registers[BANK_0][reg]) | 0x20;
        self.write_cached(out, BANK_0, reg as u32, bank0_key);
        let bank1_key = u32::from(self.registers[BANK_1][reg]) | 0x20;
        self.write_cached(out, BANK_1, reg as u32, bank1_key);
    }

    /// `FM4_TonesApplyVolume`, 0x10688.
    fn apply_volume(&mut self, out: &mut dyn OplWriter, voice: usize, volume: i32) {
        for bank in 0..2 {
            let v = self.voices[voice];
            let carrier = ATTENUATION[((volume * (v.carrier_base[bank] + 1)) >> 7) as usize];
            self.set_operator_level(out, bank, voice, ToneField::CarrierLevel, carrier);
            if v.algorithm[bank] == 1 {
                let modulator =
                    ATTENUATION[((volume * (v.modulator_base[bank] + 1)) >> 7) as usize];
                self.set_operator_level(out, bank, voice, ToneField::ModulatorLevel, modulator);
            }
        }
    }

    /// `FM4_OpProgramChange`, 0x100e7.
    #[doc(alias = "im_fm4_driver_program_change")]
    pub fn program_change(&mut self, channel: i32, program: i32) {
        if valid_channel(channel) {
            self.channels[channel as usize].program = program.clamp(0, 127);
        }
    }

    /// `FM4_OpNoteOn`, 0x100fb. Takes the oldest free voice, or steals one from an overflowing or lower-priority
    /// channel. Returns the voice, or -1.
    #[doc(alias = "im_fm4_driver_note_on")]
    pub fn note_on(
        &mut self,
        out: &mut dyn OplWriter,
        channel: i32,
        note: i32,
        velocity: i32,
    ) -> i32 {
        if !valid_channel(channel) {
            return -1;
        }
        let note = note.clamp(0, 127);
        let velocity = velocity.clamp(0, 127);
        let ch = channel as usize;
        if channel == PERCUSSION_CHANNEL {
            self.channels[ch].program = i32::from(PERCUSSION_TONES[note as usize]) + 128;
            self.channels[ch].pitch_bend = PERCUSSION_PITCH[note as usize] + 0x2000;
        }

        let mut voice = self.list_tail(self.free_head);
        if voice != INVALID_VOICE {
            let mut head = self.free_head;
            self.list_remove(&mut head, voice);
            self.free_head = head;
        } else {
            let mut selected = INVALID_VOICE;
            let mut selected_channel = self.channels[ch];
            let mut candidate = self.active_head;
            while candidate != INVALID_VOICE {
                let candidate_channel = self.channels
                    [usize::from(self.voice_nodes[usize::from(candidate)].midi_channel)];
                let replace = if candidate_channel.overflow == selected_channel.overflow {
                    candidate_channel.priority <= selected_channel.priority
                } else {
                    candidate_channel.overflow
                };
                if replace {
                    selected = candidate;
                    selected_channel = candidate_channel;
                }
                candidate = self.voice_nodes[usize::from(candidate)].next;
            }
            if selected == INVALID_VOICE {
                return -1;
            }
            voice = selected;
            self.key_off_voice(out, usize::from(voice));
            let stolen = usize::from(self.voice_nodes[usize::from(voice)].midi_channel);
            self.channels[stolen].voice_count -= 1;
            self.channels[stolen].overflow =
                self.channels[stolen].voice_count > self.channels[stolen].note_request;
            let mut head = self.active_head;
            self.list_remove(&mut head, voice);
            self.active_head = head;
        }

        let node = &mut self.voice_nodes[usize::from(voice)];
        node.midi_channel = channel as u8;
        node.note = note as u8;
        let mut head = self.active_head;
        self.list_add(&mut head, voice);
        self.active_head = head;
        let state = &mut self.channels[ch];
        state.voice_count += 1;
        state.overflow = state.voice_count > state.note_request;
        let ChannelState {
            program,
            volume,
            pitch_bend,
            ..
        } = *state;
        self.start_voice(
            out,
            usize::from(voice),
            note,
            velocity,
            program,
            volume,
            pitch_bend,
        );
        i32::from(voice)
    }

    /// `FM4_OpNoteOff`, 0x10239: releases the first voice playing the note.
    #[doc(alias = "im_fm4_driver_note_off")]
    pub fn note_off(&mut self, out: &mut dyn OplWriter, channel: i32, note: i32) -> bool {
        if !valid_channel(channel) {
            return false;
        }
        let note = note.clamp(0, 127);
        let mut voice = self.active_head;
        while voice != INVALID_VOICE {
            let node = self.voice_nodes[usize::from(voice)];
            if i32::from(node.midi_channel) != channel || i32::from(node.note) != note {
                voice = node.next;
                continue;
            }
            self.key_off_voice(out, usize::from(voice));
            let state = &mut self.channels[channel as usize];
            state.voice_count -= 1;
            state.overflow = state.voice_count > state.note_request;
            let mut head = self.active_head;
            self.list_remove(&mut head, voice);
            self.active_head = head;
            let mut head = self.free_head;
            self.list_add(&mut head, voice);
            self.free_head = head;
            return true;
        }
        false
    }

    /// `FM4_OpControlChange`, 0x102a8: volume, pan, note request and priority.
    #[doc(alias = "im_fm4_driver_control_change")]
    pub fn control_change(
        &mut self,
        out: &mut dyn OplWriter,
        channel: i32,
        controller: u32,
        value: i32,
    ) {
        if !valid_channel(channel) {
            return;
        }
        let value = value.clamp(0, 127);
        let state = &mut self.channels[channel as usize];
        match controller {
            CONTROLLER_VOLUME => {
                state.volume = value;
                let mut voice = self.active_head;
                while voice != INVALID_VOICE {
                    let node = self.voice_nodes[usize::from(voice)];
                    if i32::from(node.midi_channel) == channel {
                        self.apply_volume(out, usize::from(voice), value);
                    }
                    voice = node.next;
                }
            }
            CONTROLLER_PAN => state.pan = value,
            CONTROLLER_NOTE_REQUEST => state.note_request = value,
            CONTROLLER_PRIORITY => state.priority = value,
            _ => {}
        }
    }

    /// `FM4_OpPitchBend`, 0x10321. The percussion channel ignores bends.
    #[doc(alias = "im_fm4_driver_pitch_bend")]
    pub fn pitch_bend(&mut self, out: &mut dyn OplWriter, channel: i32, pitch_bend14: i32) {
        if !valid_channel(channel) || channel == PERCUSSION_CHANNEL {
            return;
        }
        let pitch_bend14 = pitch_bend14.clamp(0, 0x3fff);
        self.channels[channel as usize].pitch_bend = pitch_bend14;
        let mut voice = self.active_head;
        while voice != INVALID_VOICE {
            let node = self.voice_nodes[usize::from(voice)];
            if i32::from(node.midi_channel) == channel {
                self.program_pitch(
                    out,
                    usize::from(voice),
                    i32::from(node.note),
                    shift_right_signed(pitch_bend14 - 0x2000, 1),
                );
            }
            voice = node.next;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Recorder(Vec<(u16, u8)>);

    impl OplWriter for Recorder {
        fn write(&mut self, reg: u16, value: u8) {
            self.0.push((reg, value));
        }
    }

    #[test]
    fn init_clears_both_banks_then_sets_the_mode_registers() {
        let mut out = Recorder::default();
        let _driver = Fm4Driver::new(&mut out);
        assert_eq!(out.0.len(), 512 + 2);
        assert_eq!(&out.0[512..], &[(0x08, 0x40), (0x105, 0x01)]);
    }

    #[test]
    fn note_on_keys_a_voice_and_note_off_releases_it() {
        let mut out = Recorder::default();
        let mut driver = Fm4Driver::new(&mut out);
        out.0.clear();
        assert_eq!(
            driver.note_on(&mut out, 0, 60, 100),
            0,
            "the free list hands out its tail first"
        );
        let key_on = out
            .0
            .iter()
            .filter(|&&(reg, value)| reg == 0xb0 && value & 0x20 != 0)
            .count();
        assert_eq!(key_on, 1);
        out.0.clear();
        assert!(driver.note_off(&mut out, 0, 60));
        assert_eq!(out.0, [(0xb0, out.0[0].1), (0x1b0, out.0[1].1)]);
        assert_eq!(out.0[0].1 & 0x20, 0);
        assert!(!driver.note_off(&mut out, 0, 60));
    }
}
