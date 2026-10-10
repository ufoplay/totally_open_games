//! Port of `gmidi_driver.c`, `internal/gmidi_driver.h`: the General MIDI driver of the SoundFont backend, with
//! LucasArts' 12-voice allocator.

/// Receives the driver's MIDI messages. Returns 0 when accepted, -1 otherwise.
#[doc(alias = "ImGmidiWriteMessageFunc")]
pub(crate) trait MidiSink {
    #[doc(alias = "send_message")]
    fn write_message(&mut self, bytes: &[u8]) -> i32;
}

#[doc(alias = "IM_GMIDI_CHANNEL_COUNT")]
pub(crate) const CHANNEL_COUNT: usize = 16;
#[doc(alias = "IM_GMIDI_VOICE_COUNT")]
const VOICE_COUNT: usize = 12;
#[doc(alias = "GMIDI_INVALID_VOICE")]
const INVALID_VOICE: i8 = -1;

/// Roland GS reset-style SysEx sent at init.
#[doc(alias = "g_roland_init")]
const ROLAND_INIT: [u8; 26] = [
    0xf0, 0x41, 0x10, 0x42, 0x12, 0x40, 0x01, 0x10, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01,
    0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x17, 0xf7,
];

#[doc(alias = "ImGmidiChannelState")]
#[derive(Debug, Clone, Copy, Default)]
struct ChannelState {
    /// CC 18.
    priority: i32,
    /// CC 17.
    note_request: i32,
    voice_count: i32,
    overflow: bool,
}

#[doc(alias = "ImGmidiVoice")]
#[derive(Debug, Clone, Copy, Default)]
struct Voice {
    previous: i8,
    next: i8,
    /// Bit 7 marks a release that failed and is still pending.
    channel: u8,
    note: u8,
}

#[doc(alias = "ImGmidiDriver")]
#[derive(Debug, Clone)]
pub(crate) struct GmidiDriver {
    channels: [ChannelState; CHANNEL_COUNT],
    voices: [Voice; VOICE_COUNT],
    active_head: i8,
    free_head: i8,
}

fn valid_channel(channel: i32) -> bool {
    (channel as u32) < CHANNEL_COUNT as u32
}

fn send_control(out: &mut dyn MidiSink, channel: i32, controller: i32, value: i32) -> i32 {
    out.write_message(&[0xb0 | channel as u8, controller as u8, value as u8])
}

impl GmidiDriver {
    /// Sends the GS init and each channel's defaults, and frees every voice.
    #[doc(alias = "im_gmidi_driver_init")]
    pub fn new(out: &mut dyn MidiSink) -> Self {
        let mut driver = Self {
            channels: [ChannelState::default(); CHANNEL_COUNT],
            voices: [Voice::default(); VOICE_COUNT],
            active_head: INVALID_VOICE,
            free_head: INVALID_VOICE,
        };
        out.write_message(&ROLAND_INIT);
        for channel in 0..CHANNEL_COUNT as i32 {
            send_control(out, channel, 0, 0);
            driver.program_change(out, channel, 0);
            send_control(out, channel, 100, 0);
            send_control(out, channel, 101, 0);
            send_control(out, channel, 6, 16);
            send_control(out, channel, 64, 0);
            send_control(out, channel, 123, 0);
            send_control(out, channel, 91, 64);
            send_control(out, channel, 93, 0);
            driver.channels[channel as usize].priority = 0;
            driver.channels[channel as usize].note_request = 1;
        }
        for voice in 0..VOICE_COUNT as i8 {
            driver.voices[voice as usize].previous = INVALID_VOICE;
            driver.voices[voice as usize].next = INVALID_VOICE;
            let mut head = driver.free_head;
            driver.list_add(&mut head, voice);
            driver.free_head = head;
        }
        driver
    }

    /// Releases the pedal and every note on each channel.
    #[doc(alias = "im_gmidi_driver_deinit")]
    pub fn deinit(&mut self, out: &mut dyn MidiSink) {
        for channel in 0..CHANNEL_COUNT as i32 {
            send_control(out, channel, 64, 0);
            send_control(out, channel, 123, 0);
        }
    }

    fn list_remove(&mut self, head: &mut i8, index: i8) {
        let Voice { previous, next, .. } = self.voices[index as usize];
        if previous != INVALID_VOICE {
            self.voices[previous as usize].next = next;
        } else {
            *head = next;
        }
        if next != INVALID_VOICE {
            self.voices[next as usize].previous = previous;
        }
        let voice = &mut self.voices[index as usize];
        voice.previous = INVALID_VOICE;
        voice.next = INVALID_VOICE;
    }

    fn list_add(&mut self, head: &mut i8, index: i8) {
        let voice = &mut self.voices[index as usize];
        voice.previous = INVALID_VOICE;
        voice.next = *head;
        if *head != INVALID_VOICE {
            self.voices[*head as usize].previous = index;
        }
        *head = index;
    }

    fn release_voice(&mut self, index: i8) {
        let channel = usize::from(self.voices[index as usize].channel & 0x7f);
        let state = &mut self.channels[channel];
        state.voice_count -= 1;
        state.overflow = state.voice_count > state.note_request;
        let mut head = self.active_head;
        self.list_remove(&mut head, index);
        self.active_head = head;
        let mut head = self.free_head;
        self.list_add(&mut head, index);
        self.free_head = head;
    }

    #[doc(alias = "im_gmidi_driver_program_change")]
    pub fn program_change(&mut self, out: &mut dyn MidiSink, channel: i32, program: i32) -> i32 {
        if !valid_channel(channel) {
            return -1;
        }
        out.write_message(&[0xc0 | channel as u8, program as u8])
    }

    /// The voice to steal for `channel`: the last eligible one on the active list.
    fn choose_voice_to_steal(&self, channel: usize) -> i8 {
        let mut selected = INVALID_VOICE;
        let mut selected_state = self.channels[channel];
        let mut current = self.active_head;
        while current != INVALID_VOICE {
            let voice = self.voices[current as usize];
            let candidate = self.channels[usize::from(voice.channel & 0x7f)];
            let eligible = if candidate.overflow == selected_state.overflow {
                candidate.priority <= selected_state.priority
            } else {
                candidate.overflow
            };
            if eligible {
                selected = current;
                selected_state = candidate;
            }
            current = voice.next;
        }
        selected
    }

    /// Allocates a voice (stealing one when all twelve sound) and sends the NoteOn with the scaled velocity.
    #[doc(alias = "im_gmidi_driver_note_on")]
    pub fn note_on(
        &mut self,
        out: &mut dyn MidiSink,
        channel: i32,
        note: i32,
        velocity: i32,
    ) -> i32 {
        if !valid_channel(channel) {
            return -1;
        }
        let mut index = self.free_head;
        if index != INVALID_VOICE {
            let mut head = self.free_head;
            self.list_remove(&mut head, index);
            self.free_head = head;
        } else {
            index = self.choose_voice_to_steal(channel as usize);
            if index == INVALID_VOICE {
                return -1;
            }
            let victim = self.voices[index as usize];
            let release = [0x80 | (victim.channel & 0x7f), victim.note, 64];
            let mut result = -1;
            for _ in 0..10 {
                if result != -1 {
                    break;
                }
                result = out.write_message(&release);
            }
            if result == -1 {
                return -1;
            }
            let state = &mut self.channels[usize::from(victim.channel & 0x7f)];
            state.voice_count -= 1;
            state.overflow = state.voice_count > state.note_request;
            let mut head = self.active_head;
            self.list_remove(&mut head, index);
            self.active_head = head;
        }
        let voice = &mut self.voices[index as usize];
        voice.channel = channel as u8;
        voice.note = note as u8;
        let mut head = self.active_head;
        self.list_add(&mut head, index);
        self.active_head = head;
        let state = &mut self.channels[channel as usize];
        state.voice_count += 1;
        state.overflow = state.voice_count > state.note_request;
        out.write_message(&[
            0x90 | channel as u8,
            note as u8,
            (((110 * velocity) >> 7) + 17) as u8,
        ])
    }

    /// Releases one voice playing the note, or one whose release is still pending.
    #[doc(alias = "im_gmidi_driver_note_off")]
    pub fn note_off(&mut self, out: &mut dyn MidiSink, channel: i32, note: i32) -> i32 {
        if !valid_channel(channel) {
            return -1;
        }
        let mut result = 0;
        let mut current = self.active_head;
        while current != INVALID_VOICE {
            let voice = self.voices[current as usize];
            if voice.channel & 0x80 == 0
                && (i32::from(voice.channel) != channel || i32::from(voice.note) != note)
            {
                current = voice.next;
                continue;
            }
            let message = [0x80 | (voice.channel & 0x7f), voice.note, 64];
            result = -1;
            // The original loop performs nine sends: counters 1 through 9.
            for _ in 1..10 {
                if result != -1 {
                    break;
                }
                result = out.write_message(&message);
            }
            if result == -1 {
                self.voices[current as usize].channel |= 0x80;
                return -1;
            }
            self.voices[current as usize].channel &= 0x7f;
            self.release_voice(current);
            // Retail leaves the old active chain after moving this node to the free list, so one call retires one
            // matching or pending voice.
            return result;
        }
        result
    }

    /// Forwards modulation, volume, pan, reverb and chorus; keeps note request and priority.
    #[doc(alias = "im_gmidi_driver_control_change")]
    pub fn control_change(
        &mut self,
        out: &mut dyn MidiSink,
        channel: i32,
        controller: u32,
        value: i32,
    ) -> i32 {
        if !valid_channel(channel) {
            return -1;
        }
        match controller {
            1 | 7 | 10 | 91 | 93 => send_control(out, channel, controller as i32, value),
            17 => {
                self.channels[channel as usize].note_request = value;
                value
            }
            18 => {
                self.channels[channel as usize].priority = value;
                value
            }
            _ => controller as i32,
        }
    }

    #[doc(alias = "im_gmidi_driver_pitch_bend")]
    pub fn pitch_bend(&mut self, out: &mut dyn MidiSink, channel: i32, bend14: i32) -> i32 {
        if !valid_channel(channel) {
            return -1;
        }
        out.write_message(&[
            0xe0 | channel as u8,
            (bend14 & 0x7f) as u8,
            ((bend14 >> 7) & 0x7f) as u8,
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Recorder(Vec<Vec<u8>>);

    impl MidiSink for Recorder {
        fn write_message(&mut self, bytes: &[u8]) -> i32 {
            self.0.push(bytes.to_vec());
            0
        }
    }

    #[test]
    fn steals_the_last_eligible_voice_when_twelve_sound() {
        let mut out = Recorder::default();
        let mut driver = GmidiDriver::new(&mut out);
        out.0.clear();
        for note in 0..12 {
            driver.note_on(&mut out, 0, note, 127);
        }
        assert_eq!(out.0[0], [0x90, 0, 126], "velocity is scaled to 17..=126");
        out.0.clear();
        driver.note_on(&mut out, 0, 60, 100);
        // The active list is newest first, so its last eligible voice holds the oldest note.
        assert_eq!(out.0, [vec![0x80, 0, 64], vec![0x90, 60, 102]]);
        out.0.clear();
        assert_eq!(driver.note_off(&mut out, 0, 60), 0);
        assert_eq!(out.0, [vec![0x80, 60, 64]]);
    }
}
