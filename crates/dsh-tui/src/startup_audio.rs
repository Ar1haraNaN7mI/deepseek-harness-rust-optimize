//! Original electronic cues mixed with fixed offline narration in one audio stream.
//!
//! Windows uses WinMM; macOS and Linux use an installed system audio player.
//! Missing devices/players simply mute the sequence; playback never blocks the UI.

use std::f32::consts::TAU;

const SAMPLE_RATE: u32 = 24_000;
const MAX_AMPLITUDE: f32 = 0.42;

#[derive(Clone, Copy, Debug)]
pub(super) enum Cue {
    Ignite,
    Scan,
    Focus,
    Route,
    Resolve,
    Ready,
}

impl Cue {
    fn duration_ms(self) -> u32 {
        match self {
            Self::Ignite => 1_400,
            Self::Scan => 1_050,
            Self::Focus => 1_300,
            Self::Route => 1_400,
            Self::Resolve => 1_000,
            Self::Ready => 1_500,
        }
    }
}

pub(super) struct StartupAudio {
    muted: bool,
    volume: f32,
    player: platform::Player,
    spoken_until: Option<std::time::Instant>,
}

impl StartupAudio {
    pub(super) fn new(enabled: bool, volume: f32) -> Self {
        Self {
            muted: !enabled,
            volume: safe_volume(volume),
            player: platform::Player::default(),
            spoken_until: None,
        }
    }

    pub(super) fn play(&mut self, cue: Cue) {
        self.spoken_until = None;
        if self.muted || self.volume == 0.0 {
            return;
        }
        if !self.player.play(synthesize(cue, self.volume)) {
            // Keep the UI's mute indicator honest when an output is unavailable.
            self.muted = true;
        }
    }

    /// Mix a fixed, generated recording and the electronic cue into one stream.
    /// A single player avoids WinMM's process-wide PlaySound stream replacing itself.
    pub(super) fn play_with_voice(&mut self, cue: Cue, voice: Option<&[u8]>) {
        let Some(voice) = voice else {
            self.play(cue);
            return;
        };
        self.spoken_until = None;
        if self.muted || self.volume == 0.0 {
            return;
        }
        let Some((wave, spoken_duration)) = mix_fixed_voice(cue, voice, self.volume) else {
            self.play(cue);
            return;
        };
        if self.player.play(wave) {
            self.spoken_until = Some(std::time::Instant::now() + spoken_duration);
        } else {
            self.muted = true;
        }
    }

    pub(super) fn is_speaking(&self) -> bool {
        self.spoken_until
            .is_some_and(|until| std::time::Instant::now() < until)
    }

    pub(super) fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
        if muted {
            self.stop();
        }
    }

    pub(super) fn is_muted(&self) -> bool {
        self.muted
    }

    pub(super) fn stop(&mut self) {
        self.player.stop();
        self.spoken_until = None;
    }
}

fn voice_pcm(wave: &[u8]) -> Option<&[u8]> {
    if wave.get(..4)? != b"RIFF" || wave.get(8..12)? != b"WAVE" {
        return None;
    }
    let end = (u32::from_le_bytes(wave.get(4..8)?.try_into().ok()?) as usize).checked_add(8)?;
    if end > wave.len() {
        return None;
    }
    let mut offset = 12usize;
    let mut valid_format = false;
    let mut data = None;
    while offset.checked_add(8)? <= end {
        let length =
            u32::from_le_bytes(wave.get(offset + 4..offset + 8)?.try_into().ok()?) as usize;
        let start = offset.checked_add(8)?;
        let finish = start.checked_add(length)?;
        if finish > end {
            return None;
        }
        let chunk = wave.get(start..finish)?;
        match wave.get(offset..offset + 4)? {
            b"fmt " => {
                if chunk.len() < 16 {
                    return None;
                }
                valid_format = chunk[0..4] == [1, 0, 1, 0]
                    && u32::from_le_bytes(chunk[4..8].try_into().ok()?) == SAMPLE_RATE
                    && u32::from_le_bytes(chunk[8..12].try_into().ok()?) == SAMPLE_RATE * 2
                    && chunk[12..16] == [2, 0, 16, 0];
            }
            b"data" => data = Some(chunk),
            _ => {}
        }
        offset = finish.checked_add(length % 2)?;
    }
    let data = data?;
    (valid_format && !data.is_empty() && data.len() % 2 == 0).then_some(data)
}

fn mix_fixed_voice(cue: Cue, voice: &[u8], volume: f32) -> Option<(Vec<u8>, std::time::Duration)> {
    let voice = voice_pcm(voice)?;
    let effect_wave = synthesize(cue, 1.0);
    let effect = &effect_wave[44..];
    let length = voice.len().max(effect.len());
    let mut wave = effect_wave[..44].to_vec();
    wave[4..8].copy_from_slice(&(36u32.checked_add(u32::try_from(length).ok()?)?).to_le_bytes());
    wave[40..44].copy_from_slice(&u32::try_from(length).ok()?.to_le_bytes());
    wave.reserve(length);
    let sample = |bytes: &[u8], index: usize| -> f32 {
        bytes
            .get(index..index + 2)
            .map(|pair| i16::from_le_bytes([pair[0], pair[1]]) as f32)
            .unwrap_or(0.0)
    };
    let volume = safe_volume(volume);
    for index in (0..length).step_by(2) {
        let mixed = ((sample(voice, index) + sample(effect, index) * 0.2) * volume)
            .clamp(i16::MIN as f32, i16::MAX as f32) as i16;
        wave.extend_from_slice(&mixed.to_le_bytes());
    }
    Some((
        wave,
        std::time::Duration::from_secs_f64(voice.len() as f64 / 2.0 / f64::from(SAMPLE_RATE)),
    ))
}

impl Drop for StartupAudio {
    fn drop(&mut self) {
        self.stop();
    }
}

fn safe_volume(volume: f32) -> f32 {
    if volume.is_finite() {
        volume.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn sine(hz: f32, time: f32) -> f32 {
    (TAU * hz * time).sin()
}

fn smoothstep(value: f32) -> f32 {
    let value = value.clamp(0.0, 1.0);
    value * value * (3.0 - 2.0 * value)
}

fn envelope(time: f32, duration: f32, attack: f32, release: f32) -> f32 {
    smoothstep(time / attack) * smoothstep((duration - time) / release)
}

fn bell(time: f32, start: f32, frequency: f32, decay: f32) -> f32 {
    if time < start {
        return 0.0;
    }
    let age = time - start;
    smoothstep(age / 0.008)
        * (-decay * age).exp()
        * (sine(frequency, age) + 0.16 * sine(frequency * 2.003, age))
}

fn sweep(time: f32, start: f32, length: f32, from: f32, to: f32) -> f32 {
    let age = time - start;
    if age < 0.0 || age >= length {
        return 0.0;
    }
    // Integrating frequency keeps the sweep continuous through its whole arc.
    let phase = from * age + (to - from) * age * age / (2.0 * length);
    (TAU * phase).sin() * envelope(age, length, (length * 0.12).min(0.07), length * 0.35)
}

fn pad(time: f32, start: f32, length: f32, frequency: f32) -> f32 {
    let age = time - start;
    if age < 0.0 || age >= length {
        return 0.0;
    }
    envelope(age, length, 0.12, 0.32)
        * (sine(frequency, age)
            + 0.15 * sine(frequency * 2.0, age)
            + 0.04 * sine(frequency * 3.0, age))
}

fn synthesize(cue: Cue, volume: f32) -> Vec<u8> {
    let volume = safe_volume(volume);
    let sample_count = SAMPLE_RATE * cue.duration_ms() / 1_000;
    let data_length = sample_count * 2;
    let mut wave = Vec::with_capacity(44 + data_length as usize);
    wave.extend_from_slice(b"RIFF");
    wave.extend_from_slice(&(36 + data_length).to_le_bytes());
    wave.extend_from_slice(b"WAVEfmt ");
    wave.extend_from_slice(&16_u32.to_le_bytes()); // PCM format chunk length.
    wave.extend_from_slice(&1_u16.to_le_bytes()); // Linear PCM.
    wave.extend_from_slice(&1_u16.to_le_bytes()); // Mono.
    wave.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    wave.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
    wave.extend_from_slice(&2_u16.to_le_bytes()); // Block alignment.
    wave.extend_from_slice(&16_u16.to_le_bytes());
    wave.extend_from_slice(b"data");
    wave.extend_from_slice(&data_length.to_le_bytes());

    let duration = (sample_count - 1) as f32 / SAMPLE_RATE as f32;
    // A repeatable, softly band-limited air layer adds motion without hiss or
    // external samples. The filters remove both sharp highs and low-frequency DC.
    let mut noise_state = 0x51A7_9E3D_u32;
    let mut noise_fast = 0.0_f32;
    let mut noise_slow = 0.0_f32;
    let mut previous_input = 0.0_f32;
    let mut previous_output = 0.0_f32;
    let dc_decay = (-TAU * 10.0 / SAMPLE_RATE as f32).exp();
    for index in 0..sample_count {
        let time = index as f32 / SAMPLE_RATE as f32;
        noise_state ^= noise_state << 13;
        noise_state ^= noise_state >> 17;
        noise_state ^= noise_state << 5;
        let white = (noise_state >> 8) as f32 / 8_388_607.5 - 1.0;
        noise_fast += 0.30 * (white - noise_fast);
        noise_slow += 0.045 * (white - noise_slow);
        let air = noise_fast - noise_slow;
        let (signal, attack, release) = match cue {
            Cue::Ignite => {
                // Sub-bass charges first; two rising layers and filtered air
                // open the frame before a quiet return tone establishes pitch.
                (
                    0.43 * pad(time, 0.0, 1.36, 55.0)
                        + 0.18 * pad(time, 0.08, 1.25, 110.0)
                        + 0.24 * sweep(time, 0.08, 1.15, 96.0, 720.0)
                        + 0.10 * sweep(time, 0.23, 1.05, 90.0, 660.0)
                        + 0.15 * air * envelope(time, 1.34, 0.44, 0.45)
                        + 0.12 * bell(time, 0.98, 261.626, 5.0),
                    0.16,
                    0.30,
                )
            }
            Cue::Scan => {
                // Small encoded particles travel through a rounded eight-note
                // sequence. A low bed keeps their transients from sounding thin.
                let particles = [
                    (0.04, 523.251),
                    (0.15, 659.255),
                    (0.26, 783.991),
                    (0.38, 1_046.502),
                    (0.49, 783.991),
                    (0.61, 659.255),
                    (0.73, 987.767),
                    (0.85, 1_046.502),
                ]
                .into_iter()
                .map(|(start, frequency)| 0.23 * bell(time, start, frequency, 22.0))
                .sum::<f32>();
                (
                    particles
                        + 0.10 * pad(time, 0.0, 1.0, 196.0)
                        + 0.035 * air * envelope(time, 1.0, 0.1, 0.25),
                    0.018,
                    0.16,
                )
            }
            Cue::Focus => {
                // A pair of nearby frequencies converge, then the locked root,
                // fifth and octave bloom with restrained delayed reflections.
                (
                    0.18 * sweep(time, 0.0, 0.55, 505.0, 523.251)
                        + 0.18 * sweep(time, 0.0, 0.55, 542.0, 523.251)
                        + 0.25 * bell(time, 0.39, 523.251, 3.7)
                        + 0.18 * bell(time, 0.43, 783.991, 4.1)
                        + 0.13 * bell(time, 0.47, 1_046.502, 4.6)
                        + 0.08 * bell(time, 0.73, 523.251, 5.0)
                        + 0.06 * bell(time, 0.81, 783.991, 5.5)
                        + 0.11 * pad(time, 0.17, 1.05, 130.813),
                    0.06,
                    0.26,
                )
            }
            Cue::Route => {
                // Crossing sweeps suggest passing through a corridor. Filtered
                // air widens the motion; a low carrier and arrival fifth anchor it.
                (
                    0.24 * sweep(time, 0.02, 1.08, 180.0, 880.0)
                        + 0.14 * sweep(time, 0.19, 1.03, 1_000.0, 220.0)
                        + 0.18 * pad(time, 0.0, 1.32, 110.0)
                        + 0.28 * air * envelope(time, 1.32, 0.36, 0.48)
                        + 0.15 * bell(time, 0.98, 391.995, 5.5)
                        + 0.10 * bell(time, 1.06, 783.991, 7.0),
                    0.05,
                    0.24,
                )
            }
            Cue::Resolve => {
                // A damped low impact closes the mechanism; short metallic
                // partials latch, followed by a gentle two-tone validation echo.
                (
                    0.48 * sweep(time, 0.0, 0.23, 180.0, 52.0)
                        + 0.32 * bell(time, 0.04, 65.406, 5.0)
                        + 0.16 * bell(time, 0.05, 284.0, 18.0)
                        + 0.11 * bell(time, 0.075, 426.0, 21.0)
                        + 0.07 * bell(time, 0.10, 709.0, 24.0)
                        + 0.14 * air * envelope(time, 0.15, 0.008, 0.11)
                        + 0.17 * bell(time, 0.34, 659.255, 5.5)
                        + 0.12 * bell(time, 0.44, 987.767, 6.5),
                    0.012,
                    0.24,
                )
            }
            Cue::Ready => {
                // The mark lands on a soft sub impact and a spacious C-add9
                // voicing. Staggered upper voices leave an original, warm tail.
                (
                    0.28 * sweep(time, 0.0, 0.26, 96.0, 48.0)
                        + 0.26 * pad(time, 0.03, 1.42, 130.813)
                        + 0.17 * pad(time, 0.09, 1.35, 195.998)
                        + 0.20 * pad(time, 0.13, 1.30, 261.626)
                        + 0.14 * pad(time, 0.18, 1.24, 329.628)
                        + 0.12 * pad(time, 0.23, 1.18, 587.33)
                        + 0.12 * bell(time, 0.45, 523.251, 3.3)
                        + 0.08 * bell(time, 0.62, 1_046.502, 4.0),
                    0.03,
                    0.32,
                )
            }
        };
        // Soft mixing can rectify closely related low tones. A 10 Hz DC
        // blocker removes that offset while retaining the bass fundamentals.
        let limited = signal.tanh();
        let centered = limited - previous_input + dc_decay * previous_output;
        previous_input = limited;
        previous_output = centered;
        let amplitude = centered.clamp(-1.0, 1.0)
            * envelope(time, duration, attack, release)
            * MAX_AMPLITUDE
            * volume;
        let sample = (amplitude * i16::MAX as f32).round() as i16;
        wave.extend_from_slice(&sample.to_le_bytes());
    }
    wave
}

#[cfg(windows)]
mod platform {
    use std::ffi::c_void;
    use std::ptr;

    const SND_ASYNC: u32 = 0x0001;
    const SND_NODEFAULT: u32 = 0x0002;
    const SND_MEMORY: u32 = 0x0004;

    #[link(name = "winmm")]
    unsafe extern "system" {
        fn PlaySoundW(sound: *const u16, module: *mut c_void, flags: u32) -> i32;
    }

    #[derive(Default)]
    pub(super) struct Player {
        // WinMM borrows this allocation until stop returns; never resize it.
        wave: Option<Vec<u8>>,
    }

    impl Player {
        pub(super) fn play(&mut self, wave: Vec<u8>) -> bool {
            self.stop();
            self.wave = Some(wave);
            let pointer = self.wave.as_ref().unwrap().as_ptr().cast::<u16>();
            // SAFETY: SND_MEMORY interprets `sound` as a complete WAV buffer.
            // Its allocation stays alive and unchanged in `self.wave` until a
            // subsequent synchronous stop, including on StartupAudio::drop.
            // WinMM contract: https://learn.microsoft.com/en-us/previous-versions/dd743680(v=vs.85)
            let played = unsafe {
                PlaySoundW(
                    pointer,
                    ptr::null_mut(),
                    SND_ASYNC | SND_MEMORY | SND_NODEFAULT,
                ) != 0
            };
            if !played {
                self.stop();
            }
            played
        }

        pub(super) fn stop(&mut self) {
            if self.wave.is_some() {
                // SAFETY: null explicitly stops asynchronous waveform playback.
                // Release the borrowed memory only after WinMM has returned.
                unsafe { PlaySoundW(ptr::null(), ptr::null_mut(), 0) };
                self.wave = None;
            }
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod platform {
    use std::fs::{self, DirBuilder, OpenOptions};
    use std::io::{self, Write};
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    #[derive(Default)]
    pub(super) struct Player {
        child: Option<Child>,
        directory: Option<PathBuf>,
        wave_path: Option<PathBuf>,
    }

    impl Player {
        pub(super) fn play(&mut self, wave: Vec<u8>) -> bool {
            self.stop();
            let Ok(path) = self.write_wave(&wave) else {
                return false;
            };
            self.wave_path = Some(path.clone());
            if let Some(child) = spawn_player(&path) {
                self.child = Some(child);
                true
            } else {
                self.stop();
                false
            }
        }

        fn write_wave(&mut self, wave: &[u8]) -> io::Result<PathBuf> {
            if self.directory.is_none() {
                let timestamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos();
                let serial = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
                let directory = std::env::temp_dir().join(format!(
                    "dsh-startup-{}-{timestamp}-{serial}",
                    std::process::id()
                ));
                // create (not create_dir_all) rejects existing paths/symlinks.
                DirBuilder::new().mode(0o700).create(&directory)?;
                self.directory = Some(directory);
            }
            let path = self.directory.as_ref().unwrap().join("cue.wav");
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&path)?;
            if let Err(error) = file.write_all(wave) {
                drop(file);
                let _ = fs::remove_file(&path);
                return Err(error);
            }
            Ok(path)
        }

        pub(super) fn stop(&mut self) {
            if let Some(mut child) = self.child.take() {
                // Own and reap the direct player process, including on skip/mute.
                if !matches!(child.try_wait(), Ok(Some(_))) {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
            if let Some(path) = self.wave_path.take() {
                let _ = fs::remove_file(path);
            }
        }
    }

    impl Drop for Player {
        fn drop(&mut self) {
            self.stop();
            if let Some(directory) = self.directory.take() {
                let _ = fs::remove_dir(directory);
            }
        }
    }

    fn spawn_player(path: &Path) -> Option<Child> {
        #[cfg(target_os = "macos")]
        let players: &[(&str, &[&str])] = &[("/usr/bin/afplay", &[])];
        #[cfg(target_os = "linux")]
        let players: &[(&str, &[&str])] = &[("aplay", &["-q"]), ("paplay", &[])];

        for &(program, arguments) in players {
            if let Ok(child) = Command::new(program)
                .args(arguments)
                .arg(path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                return Some(child);
            }
        }
        None
    }
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
mod platform {
    #[derive(Default)]
    pub(super) struct Player;

    impl Player {
        pub(super) fn play(&mut self, _wave: Vec<u8>) -> bool {
            false
        }

        pub(super) fn stop(&mut self) {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CUES: [Cue; 6] = [
        Cue::Ignite,
        Cue::Scan,
        Cue::Focus,
        Cue::Route,
        Cue::Resolve,
        Cue::Ready,
    ];

    fn samples(wave: &[u8]) -> impl Iterator<Item = i16> + '_ {
        wave[44..]
            .chunks_exact(2)
            .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn u32_at(wave: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(wave[offset..offset + 4].try_into().unwrap())
    }

    #[test]
    fn embedded_recordings_match_their_manifest_and_keep_complete_voice_tails() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../assets/voice/manifest.json")).unwrap();
        let recordings: [(&str, &[u8]); 7] = [
            ("phase-0.wav", include_bytes!("../assets/voice/phase-0.wav")),
            ("phase-1.wav", include_bytes!("../assets/voice/phase-1.wav")),
            ("phase-2.wav", include_bytes!("../assets/voice/phase-2.wav")),
            (
                "phase-3-mounted.wav",
                include_bytes!("../assets/voice/phase-3-mounted.wav"),
            ),
            ("phase-4.wav", include_bytes!("../assets/voice/phase-4.wav")),
            ("phase-5.wav", include_bytes!("../assets/voice/phase-5.wav")),
            (
                "load-unavailable.wav",
                include_bytes!("../assets/voice/load-unavailable.wav"),
            ),
        ];
        for (name, recording) in recordings {
            let clip = manifest["clips"]
                .as_array()
                .unwrap()
                .iter()
                .find(|clip| clip["file"] == name)
                .unwrap();
            let pcm = voice_pcm(recording).expect("embedded recording must use supported PCM");
            assert_eq!(pcm.len() / 2, clip["frames"].as_u64().unwrap() as usize);
            let (mixed, duration) = mix_fixed_voice(Cue::Resolve, recording, 0.35).unwrap();
            assert!(
                (duration.as_secs_f64() - clip["duration_seconds"].as_f64().unwrap()).abs() < 1e-6
            );
            assert_eq!(mixed.len(), 44 + pcm.len());
            assert!(pcm.iter().any(|byte| *byte != 0));
            // The one-second cue ends first for every current recording. After
            // that point the whole original speech tail survives at user volume.
            for (sample, original) in samples(&mixed)
                .zip(pcm.chunks_exact(2))
                .skip(SAMPLE_RATE as usize)
            {
                let original = i16::from_le_bytes([original[0], original[1]]);
                assert!((f32::from(sample) - f32::from(original) * 0.35).abs() <= 1.0);
            }
        }
    }

    #[test]
    fn fixed_voice_preserves_the_full_tail_and_scales_the_complete_mix() {
        let voice = synthesize(Cue::Ready, 1.0);
        let (mixed, duration) = mix_fixed_voice(Cue::Resolve, &voice, 0.35).unwrap();
        assert_eq!(duration, std::time::Duration::from_millis(1_500));
        assert_eq!(mixed.len(), voice.len());
        assert_eq!(u32_at(&mixed, 4) as usize + 8, mixed.len());
        assert_eq!(u32_at(&mixed, 40) as usize + 44, mixed.len());

        // Narration is longer than the effect. Its last half second survives,
        // and the configured volume applies to voice as well as synthesis.
        let tail = SAMPLE_RATE as usize;
        for (mixed, original) in samples(&mixed).zip(samples(&voice)).skip(tail) {
            assert!((f32::from(mixed) - f32::from(original) * 0.35).abs() <= 1.0);
        }
        assert!(samples(&mixed).skip(tail).any(|sample| sample != 0));

        let (silent, _) = mix_fixed_voice(Cue::Resolve, &voice, 0.0).unwrap();
        assert!(samples(&silent).all(|sample| sample == 0));
        let short_voice = synthesize(Cue::Resolve, 1.0);
        let (long_effect, spoken) = mix_fixed_voice(Cue::Ready, &short_voice, 0.5).unwrap();
        assert_eq!(long_effect.len(), voice.len());
        assert_eq!(spoken, std::time::Duration::from_secs(1));
    }

    #[test]
    fn fixed_voice_reader_accepts_metadata_and_rejects_invalid_pcm() {
        let source = synthesize(Cue::Scan, 0.5);
        let mut metadata = source[..36].to_vec();
        metadata.extend_from_slice(b"JUNK\x03\x00\x00\x00abc\x00");
        metadata.extend_from_slice(&source[36..]);
        let riff_length = (metadata.len() - 8) as u32;
        metadata[4..8].copy_from_slice(&riff_length.to_le_bytes());
        assert_eq!(voice_pcm(&metadata), Some(&source[44..]));

        let mut stereo = source.clone();
        stereo[22] = 2;
        assert!(mix_fixed_voice(Cue::Scan, &stereo, 0.5).is_none());
        let mut wrong_rate = source.clone();
        wrong_rate[24..28].copy_from_slice(&48_000u32.to_le_bytes());
        assert!(voice_pcm(&wrong_rate).is_none());
        assert!(voice_pcm(&source[..source.len() - 1]).is_none());
        assert!(voice_pcm(b"not a wav").is_none());
    }

    #[test]
    fn muted_narration_never_opens_audio_and_clears_the_boundary_hold() {
        let voice = synthesize(Cue::Ready, 0.5);
        let mut audio = StartupAudio::new(false, 0.35);
        audio.play_with_voice(Cue::Ready, Some(&voice));
        assert!(!audio.is_speaking());
        audio.spoken_until = Some(std::time::Instant::now() + std::time::Duration::from_secs(5));
        assert!(audio.is_speaking());
        audio.set_muted(true);
        assert!(!audio.is_speaking());

        let mut silent = StartupAudio::new(true, 0.0);
        silent.play_with_voice(Cue::Ready, Some(&voice));
        assert!(!silent.is_speaking());
    }

    #[test]
    fn cues_have_valid_pcm_headers_and_exact_durations() {
        for cue in CUES {
            let wave = synthesize(cue, 1.0);
            let count = SAMPLE_RATE * cue.duration_ms() / 1_000;
            assert_eq!(&wave[0..4], b"RIFF");
            assert_eq!(&wave[8..16], b"WAVEfmt ");
            assert_eq!(u32_at(&wave, 4) as usize + 8, wave.len());
            assert_eq!(u32_at(&wave, 16), 16);
            assert_eq!(&wave[20..24], &[1, 0, 1, 0]);
            assert_eq!(u32_at(&wave, 24), SAMPLE_RATE);
            assert_eq!(u32_at(&wave, 28), SAMPLE_RATE * 2);
            assert_eq!(&wave[32..36], &[2, 0, 16, 0]);
            assert_eq!(&wave[36..40], b"data");
            assert_eq!(u32_at(&wave, 40), count * 2);
            assert_eq!(samples(&wave).count(), count as usize);
            assert_eq!(wave.len(), 44 + count as usize * 2);
        }
    }

    #[test]
    fn cues_have_faded_edges_and_bounded_nonzero_signals() {
        for cue in CUES {
            let wave = synthesize(cue, 1.0);
            let signal: Vec<_> = samples(&wave).collect();
            assert_eq!(signal.first(), Some(&0));
            assert_eq!(signal.last(), Some(&0));
            let peak = signal
                .iter()
                .map(|sample| i32::from(*sample).abs())
                .max()
                .unwrap();
            assert!(peak > 1_000, "{cue:?} must have audible content");
            assert!(peak <= (MAX_AMPLITUDE * i16::MAX as f32).ceil() as i32);
            let mean =
                signal.iter().map(|sample| f64::from(*sample)).sum::<f64>() / signal.len() as f64;
            assert!(
                mean.abs() < 10.0,
                "{cue:?} must not contain a DC offset: {mean}"
            );
        }
    }

    #[test]
    fn invalid_or_zero_volume_is_silent_and_large_volume_is_clamped() {
        for volume in [0.0, -1.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            for cue in CUES {
                assert!(samples(&synthesize(cue, volume)).all(|sample| sample == 0));
            }
        }
        for cue in CUES {
            assert_eq!(synthesize(cue, 1.0), synthesize(cue, 10.0));
            let full = synthesize(cue, 1.0);
            let half = synthesize(cue, 0.5);
            for (full, half) in samples(&full).zip(samples(&half)) {
                assert!((i32::from(full) - 2 * i32::from(half)).abs() <= 1);
            }
        }
    }

    #[test]
    fn mute_controls_do_not_start_playback() {
        let mut audio = StartupAudio::new(false, 0.35);
        assert!(audio.is_muted());
        audio.play(Cue::Ignite);
        audio.set_muted(false);
        assert!(!audio.is_muted());
        audio.set_muted(true);
        assert!(audio.is_muted());
        audio.stop();

        // Enabling a zero-volume instance also never touches the audio device.
        let mut silent = StartupAudio::new(true, 0.0);
        silent.play(Cue::Ready);
    }
}
