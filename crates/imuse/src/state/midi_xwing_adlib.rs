//! Port of the engine-thread half of `midi_xwing_adlib.c`: the nine-voice OPL2 driver recovered from DOS93
//! ADLIB.IMS 0x13f1e..0x14bca.
//!
//! The driver state stays with the sequencer, which allows an exact live-voice census; only copies of the
//! generated register writes cross to the audio thread (`render::midi_xwing_adlib`).

use super::xwing_adlib_data::{
    ADLIB_FNUM, ADLIB_MOD_MAX, ADLIB_MOD_PARAM, ADLIB_MOD_STEPS, ADLIB_NOTE, ADLIB_OCTAVE,
    ADLIB_REGISTER_MAP, ADLIB_VOLUME,
};
use crate::synth_queue::{QueueProducer, SynthEvent};

/// Modulator operator offset of each voice.
const OPERATORS: [i32; 9] = [0, 1, 2, 8, 9, 10, 16, 17, 18];

#[derive(Debug, Clone, Copy, Default)]
struct AdlibMod {
    stage: i32,
    duration: i32,
    position: i32,
    maximum: i32,
    base: i32,
    scale: i32,
    speed: i32,
    ticks: i32,
    remaining: i32,
    step: i32,
    remainder: i32,
    direction: i32,
    error: i32,
    output: i32,
    flags: u8,
    parameter: u8,
    times: [u8; 4],
    values: [u8; 4],
}

#[derive(Debug, Clone, Copy, Default)]
struct AdlibVoice {
    channel: i32,
    note: i32,
    duration: i32,
    carrier: i32,
    modulator: i32,
    pitch_offset: i32,
    sustained: i32,
    age: u32,
    patch: [u8; 30],
    mods: [AdlibMod; 2],
}

#[derive(Debug, Clone, Copy, Default)]
struct AdlibChannel {
    patch: [u8; 30],
    valid: bool,
    volume: i32,
    bend: i32,
    priority: i32,
    modulation: i32,
    sustain: i32,
}

/// One driver event, as upstream builds an `ImSynthEvent` and applies it in place.
#[derive(Debug, Clone, Copy)]
enum Event {
    On { note: u8, velocity: u8 },
    Off { note: u8 },
    Control { controller: u8, value: u8 },
    Bend { value: u16 },
    Instrument,
}

/// The driver state (`ImAdlib` without the chip, which lives in the renderer).
#[doc(alias("ImAdlib", "adlib_release"))]
#[derive(Debug)]
pub(crate) struct AdlibDriver {
    queue: QueueProducer,
    /// The queue's sample rate: 0 while closed, which drops writes.
    rate: i32,
    channels: [AdlibChannel; 32],
    voices: [AdlibVoice; 9],
    registers: [u8; 256],
    random: u8,
    next_voice: u32,
    clock: u32,
    age: u32,
}

fn clamp(v: i32, lo: i32, hi: i32) -> i32 {
    if v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}

/// DOS93 0x10A58 builds this table with column zero cleared afterward.
fn table_scale(value: i32, scale: i32) -> i32 {
    if scale != 0 {
        (value * (scale + 1)) >> 5
    } else {
        0
    }
}

fn scale_value(value: i32, scale: i32) -> i32 {
    if scale == 0 {
        return 0;
    }
    if scale == 31 {
        return value;
    }
    if (-63..=63).contains(&value) {
        let result = table_scale(value.abs(), scale.abs());
        return if (value < 0) != (scale < 0) {
            -result
        } else {
            result
        };
    }
    ((value + 1) * scale) >> 5
}

impl AdlibDriver {
    #[doc(alias = "imuse_midi_xwing_adlib_create")]
    pub fn new(queue: QueueProducer) -> Self {
        Self {
            queue,
            rate: 0,
            channels: [AdlibChannel::default(); 32],
            voices: [AdlibVoice::default(); 9],
            registers: [0; 256],
            random: 0,
            next_voice: 0,
            clock: 0,
            age: 0,
        }
    }

    /// Opens the driver at the output rate; the renderer resets its chip at the same rate.
    #[doc(alias = "adlib_open")]
    pub fn open(&mut self, rate: i32) {
        self.rate = rate;
        self.random = 1;
        self.reg_write(1, 32);
        self.reg_write(8, 64);
        self.reg_write(0xbd, 0);
        for channel in &mut self.channels {
            channel.volume = 127;
        }
        for voice in &mut self.voices {
            voice.channel = -1;
        }
    }

    #[doc(alias = "adlib_close")]
    pub fn close(&mut self) {
        self.rate = 0;
    }

    /// Caches a register and queues the write when it changes.
    fn reg_write(&mut self, reg: i32, value: i32) {
        let index = reg as usize & 0xff;
        if self.registers[index] == value as u8 {
            return;
        }
        self.registers[index] = value as u8;
        if self.rate == 0 {
            return;
        }
        let event = SynthEvent::OplWrite {
            register: reg as u8,
            value: value as u8,
        };
        if !self.queue.overflowed() && !self.queue.push(event) {
            tracing::error!(target: "imuse", "MIDI event queue overflow; output stopped");
        }
    }

    fn voice_stop(&mut self, i: usize) {
        let reg = 0xb0 + i as i32;
        self.reg_write(reg, i32::from(self.registers[reg as usize]) & !32);
        self.voices[i].channel = -1;
    }

    fn random_value(&mut self, value: i32) -> i32 {
        self.random = (self.random >> 1) ^ if self.random & 1 != 0 { 0xb8 } else { 0 };
        (i32::from(self.random) * value) >> 8
    }

    fn pitch(&mut self, i: usize) {
        let v = self.voices[i];
        let value = clamp(
            v.note * 128 + self.channels[v.channel as usize].bend + v.pitch_offset,
            4 * 128,
            127 * 128,
        );
        let n = ((value >> 7) - 4) as usize;
        let frac = (value >> 4) & 7;
        let mut octave = i32::from(ADLIB_OCTAVE[n]) * 4;
        let mut note = i32::from(ADLIB_NOTE[n]);
        let b0 = i32::from(self.registers[0xb0 + i]);
        let old = b0 & !32;
        if b0 & 32 != 0 {
            if octave > old && note < 6 {
                note += 12;
                octave -= 4;
            } else if octave < old && note > 11 {
                note -= 12;
                octave += 4;
            }
        }
        self.reg_write(
            0xa0 + i as i32,
            i32::from(ADLIB_FNUM[clamp(note * 8 + frac, 0, 143) as usize]),
        );
        self.reg_write(0xb0 + i as i32, octave | 32);
    }

    /// The register, shift, mask and inversion of patch parameter `param` on voice `i`.
    fn parameter_register(i: usize, param: i32) -> (usize, i32, i32, i32) {
        let base = if param < 13 {
            OPERATORS[i] + 3
        } else if param < 26 {
            OPERATORS[i]
        } else {
            i as i32
        };
        let index = (if param < 13 { param } else { param - 13 }) as usize * 4;
        let reg = i32::from(ADLIB_REGISTER_MAP[index]) + base;
        (
            reg as usize,
            i32::from(ADLIB_REGISTER_MAP[index + 1]),
            i32::from(ADLIB_REGISTER_MAP[index + 2]),
            i32::from(ADLIB_REGISTER_MAP[index + 3]),
        )
    }

    fn parameter(&mut self, i: usize, param: i32, mut value: i32) {
        if param == 28 || param == 29 {
            self.voices[i].pitch_offset = (value - if param == 28 { 15 } else { 383 }) * 16;
            self.pitch(i);
            return;
        }
        if !(0..=27).contains(&param) {
            return;
        }
        let (reg, shift, mask, invert) = Self::parameter_register(i, param);
        if invert != 0 {
            value = invert - value;
        }
        let current = i32::from(self.registers[reg]);
        self.reg_write(reg as i32, (current & !mask) | ((value << shift) & mask));
    }

    fn get_parameter(&self, i: usize, param: i32) -> i32 {
        match param {
            28 => return 15,
            29 => return 383,
            0..=27 => {}
            _ => return 0,
        }
        let (reg, shift, mask, invert) = Self::parameter_register(i, param);
        let value = (i32::from(self.registers[reg]) & mask) >> shift;
        if invert != 0 { invert - value } else { value }
    }

    fn volume(&mut self, i: usize) {
        let v = self.voices[i];
        let volume = self.channels[v.channel as usize].volume >> 2;
        self.parameter(
            i,
            0,
            i32::from(ADLIB_VOLUME[clamp(table_scale(v.carrier, volume), 0, 63) as usize]),
        );
        if v.patch[10] & 1 != 0 {
            let level = ADLIB_VOLUME[clamp(table_scale(v.modulator, volume), 0, 63) as usize];
            self.parameter(i, 13, i32::from(level));
        }
    }

    fn mod_stage(&mut self, i: usize, which: usize) {
        let m = self.voices[i].mods[which];
        let s = (m.stage - 1) as usize;
        let t = i32::from(m.times[s]);
        let index = clamp(table_scale(t & 127, m.speed), 0, 63) as usize;
        let mut ticks = i32::from(ADLIB_MOD_STEPS[index * 2])
            | (i32::from(ADLIB_MOD_STEPS[index * 2 + 1]) << 8);
        if t & 128 != 0 {
            ticks = self.random_value(ticks);
        }
        if ticks == 0 {
            ticks = 1;
        }
        let mut delta = 0;
        if s != 2 {
            delta = scale_value(m.maximum, (i32::from(m.values[s]) & 127) - 31);
            if m.values[s] & 128 != 0 {
                delta = self.random_value(delta);
            }
            delta = clamp(m.base + delta, 0, m.maximum) - m.base - m.position;
        }
        let m = &mut self.voices[i].mods[which];
        m.ticks = ticks;
        m.remaining = ticks;
        m.step = delta / ticks;
        m.direction = if delta < 0 { -1 } else { 1 };
        m.remainder = delta.abs() % ticks;
        m.error = 0;
    }

    /// Seeds modulator `which` of voice `i` from the nine patch bytes at `offset`.
    fn mod_init(&mut self, i: usize, which: usize, offset: usize) {
        self.voices[i].mods[which] = AdlibMod::default();
        let v = self.voices[i];
        let p = &v.patch[offset..offset + 9];
        if p[0] & 128 == 0 {
            return;
        }
        let kind = usize::from(p[0] & 15);
        if kind >= 15 {
            return;
        }
        let parameter = ADLIB_MOD_PARAM[kind];
        let base = match parameter {
            0 => v.carrier,
            13 => v.modulator,
            30 => 31,
            31 => 0,
            _ => self.get_parameter(i, i32::from(parameter)),
        };
        let m = AdlibMod {
            flags: p[0],
            parameter,
            maximum: i32::from(ADLIB_MOD_MAX[kind * 2])
                | (i32::from(ADLIB_MOD_MAX[kind * 2 + 1]) << 8),
            base,
            scale: if p[0] & 64 != 0 {
                self.channels[v.channel as usize].modulation >> 2
            } else {
                31
            },
            speed: 31,
            duration: i32::from(p[1]) * 63,
            times: [p[2], p[4], p[6], p[7]],
            values: [p[3], p[5], 0, p[8]],
            stage: 1,
            ..AdlibMod::default()
        };
        self.voices[i].mods[which] = m;
        self.mod_stage(i, which);
    }

    fn mod_tick(&mut self, i: usize, which: usize) {
        let m = &mut self.voices[i].mods[which];
        if m.stage == 0 {
            return;
        }
        if m.duration != 0 {
            m.duration -= 17;
            if m.duration <= 0 {
                m.stage = 0;
                return;
            }
        }
        m.position += m.step;
        m.error += m.remainder;
        if m.error >= m.ticks {
            m.error -= m.ticks;
            m.position += m.direction;
        }
        let output = scale_value(m.position, m.scale);
        if output != m.output {
            m.output = output;
            let value = clamp(m.base + output, 0, m.maximum);
            match m.parameter {
                0 => {
                    self.voices[i].carrier = value;
                    self.volume(i);
                }
                13 => {
                    self.voices[i].modulator = value;
                    self.volume(i);
                }
                30 => self.voices[i].mods[1 - which].scale = value,
                31 => self.voices[i].mods[1 - which].speed = value,
                parameter => self.parameter(i, i32::from(parameter), value),
            }
        }
        let m = &mut self.voices[i].mods[which];
        m.remaining -= 1;
        if m.remaining == 0 {
            m.stage += 1;
            if m.stage > 4 {
                if m.flags & 32 == 0 {
                    m.stage = 0;
                    return;
                }
                m.stage = 1;
                if m.flags & 16 != 0 {
                    let reg = 0xb0 + i;
                    self.reg_write(reg as i32, i32::from(self.registers[reg]) & !32);
                    self.reg_write(reg as i32, i32::from(self.registers[reg]) | 32);
                }
            }
            self.mod_stage(i, which);
        }
    }

    fn note_on(&mut self, channel: usize, note: i32, velocity: i32) {
        let c = self.channels[channel];
        if !c.valid {
            return;
        }
        let mut selected = None;
        let mut priority = c.priority;
        // 0x144f6: round-robin free voice; steal the oldest note of an eligible part.
        for _ in 0..9 {
            self.next_voice = (self.next_voice + 1) % 9;
            let i = self.next_voice as usize;
            let v = self.voices[i];
            if v.channel < 0 {
                selected = Some(i);
                break;
            }
            let oldest = !self
                .voices
                .iter()
                .any(|other| other.channel == v.channel && other.age < v.age);
            let voice_priority = self.channels[v.channel as usize].priority;
            if oldest && voice_priority <= priority {
                selected = Some(i);
                priority = voice_priority;
            }
        }
        let Some(selected) = selected else {
            return;
        };
        self.voice_stop(selected);
        self.age = self.age.wrapping_add(1);
        let p = c.patch;
        let modulator = clamp(
            i32::from(p[1] & 63) + table_scale(velocity >> 1, i32::from(p[4] >> 2)),
            0,
            63,
        );
        let carrier = clamp(
            i32::from(p[6] & 63) + table_scale(velocity >> 1, i32::from(p[9] >> 2)),
            0,
            63,
        );
        self.voices[selected] = AdlibVoice {
            channel: channel as i32,
            note,
            age: self.age,
            patch: p,
            duration: i32::from(p[29]) * 63,
            modulator,
            carrier,
            ..AdlibVoice::default()
        };
        let mut levels = [
            modulator,
            i32::from(ADLIB_VOLUME[table_scale(carrier, c.volume >> 2) as usize]),
        ];
        if p[10] & 1 != 0 {
            levels[0] = i32::from(ADLIB_VOLUME[table_scale(modulator, c.volume >> 2) as usize]);
        }
        for (op, level) in levels.into_iter().enumerate() {
            let r = OPERATORS[selected] + op as i32 * 3;
            let off = op * 5;
            self.reg_write(0x20 + r, i32::from(p[off]));
            self.reg_write(0x40 + r, (i32::from(p[off + 1]) | 63) - level);
            self.reg_write(0x60 + r, !i32::from(p[off + 2]));
            self.reg_write(0x80 + r, !i32::from(p[off + 3]));
            self.reg_write(0xe0 + r, i32::from(p[off + 4]));
        }
        self.reg_write(0xc0 + selected as i32, i32::from(p[10]));
        self.pitch(selected);
        self.mod_init(selected, 0, 11);
        self.mod_init(selected, 1, 20);
    }

    #[doc(alias = "adlib_apply")]
    fn apply(&mut self, channel: u8, event: Event, data: &[u8]) {
        let channel = usize::from(channel);
        if channel >= 32 {
            return;
        }
        match event {
            Event::Instrument => {
                if data.len() == 30 {
                    self.channels[channel].patch.copy_from_slice(data);
                    self.channels[channel].valid = true;
                }
                return;
            }
            Event::On { note, velocity } if velocity != 0 => {
                self.note_on(channel, i32::from(note), i32::from(velocity));
                return;
            }
            Event::Control { controller, value } => {
                let c = &mut self.channels[channel];
                match controller {
                    7 => c.volume = i32::from(value),
                    18 => c.priority = i32::from(value),
                    1 => c.modulation = i32::from(value),
                    64 => c.sustain = i32::from(value),
                    123 | 120 => c.sustain = 0,
                    _ => {}
                }
            }
            Event::Bend { value } => self.channels[channel].bend = i32::from(value as i16),
            _ => {}
        }
        let sustain = self.channels[channel].sustain;
        for i in 0..9 {
            if self.voices[i].channel != channel as i32 {
                continue;
            }
            match event {
                Event::Off { note } | Event::On { note, .. }
                    if self.voices[i].note == i32::from(note) =>
                {
                    if sustain != 0 {
                        self.voices[i].sustained = 1;
                    } else {
                        self.voice_stop(i);
                    }
                }
                Event::Control {
                    controller: 123 | 120,
                    ..
                } => self.voice_stop(i),
                Event::Control { controller: 64, .. }
                    if sustain == 0 && self.voices[i].sustained != 0 =>
                {
                    self.voice_stop(i);
                }
                Event::Control { controller: 7, .. } => self.volume(i),
                Event::Control {
                    controller: 1,
                    value,
                } => {
                    for m in &mut self.voices[i].mods {
                        if m.flags & 64 != 0 {
                            m.scale = i32::from(value >> 2);
                        }
                    }
                }
                Event::Bend { .. } => self.pitch(i),
                _ => {}
            }
        }
    }

    #[doc(alias = "adlib_on")]
    pub fn note_on_event(&mut self, channel: i32, note: i32, velocity: i32) {
        let event = Event::On {
            note: note as u8,
            velocity: velocity as u8,
        };
        self.apply(channel as u8, event, &[]);
    }

    #[doc(alias = "adlib_off")]
    pub fn note_off_event(&mut self, channel: i32, note: i32) {
        self.apply(channel as u8, Event::Off { note: note as u8 }, &[]);
    }

    #[doc(alias = "adlib_control")]
    pub fn control_event(&mut self, channel: i32, controller: u32, value: i32) {
        let event = Event::Control {
            controller: controller as u8,
            value: value as u8,
        };
        self.apply(channel as u8, event, &[]);
    }

    /// Signed pitch, 128 units per semitone.
    #[doc(alias = "adlib_bend")]
    pub fn bend(&mut self, channel: i32, detune: i32) {
        self.apply(
            channel as u8,
            Event::Bend {
                value: detune as u16,
            },
            &[],
        );
    }

    /// A 14-bit wire bend, converted to the driver's units.
    #[doc(alias = "adlib_wire_bend")]
    pub fn wire_bend(&mut self, channel: i32, value: i32) {
        let bend = value - 8192;
        self.bend(
            channel,
            if bend >= 0 {
                bend / 4
            } else {
                -((-bend + 3) / 4)
            },
        );
    }

    #[doc(alias = "adlib_instrument")]
    pub fn instrument(&mut self, channel: i32, data: &[u8]) {
        if data.len() == 30 {
            self.apply(channel as u8, Event::Instrument, data);
        }
    }

    /// Sets, for each note, the bits of the requested channels whose voices play it.
    #[doc(alias = "adlib_notes")]
    pub fn notes(&self, channels: u32, notes: &mut [u32; 128]) {
        notes.fill(0);
        for v in &self.voices {
            if v.channel >= 0 && channels & (1 << v.channel) != 0 {
                notes[v.note as usize] |= 1 << v.channel;
            }
        }
    }

    /// Runs the 60 Hz voice service on the 3433 µs sequencer clock.
    #[doc(alias = "adlib_service")]
    pub fn service(&mut self) {
        self.clock += 3433;
        if self.clock < 16667 {
            return;
        }
        self.clock -= 16667;
        for i in 0..9 {
            if self.voices[i].channel < 0 {
                continue;
            }
            let v = &mut self.voices[i];
            if v.duration != 0 {
                v.duration -= 17;
                if v.duration <= 0 {
                    self.voice_stop(i);
                    // 0x14092 stops this service after an expired voice.
                    return;
                }
            }
            self.mod_tick(i, 0);
            self.mod_tick(i, 1);
        }
    }
}
