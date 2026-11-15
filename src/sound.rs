//! Monaged's sounds: a few short cues for moments you might otherwise miss
//! because you're not looking, and nothing else.
//!
//! The rules come from how sound is used well in interfaces (Material
//! Design's sound guidelines, Google's earcon guidance, earcon research):
//!
//! - **Silence is the default.** No sound for messages, tool calls, typing,
//!   clicks or anything that happens often. Agents work in the background;
//!   sound is for when they need you or are done while you look elsewhere.
//! - **A small vocabulary.** Five cues, each with its own shape, used the
//!   same way every time, so they're learned quickly: rising for good news,
//!   falling for trouble, brighter and repeated for "needs you".
//! - **A loudness order.** Alerts are loudest, everyday feedback quietest.
//! - **Never twice in a row.** Bursts (four builders finishing together) are
//!   one sound; a more important cue wins over a less important one.
//! - **Respect where you are.** Nothing in presentation mode, a full-screen
//!   app, while Windows' notifications are held back, or while you're away.
//!   Off, softer, louder or per cue in Settings → Sounds, and Ctrl K → Mute.
//! - **Not Windows' own sounds.** The cues are made here, so they never
//!   sound like a system alert; while they're on, Monaged's Windows
//!   notifications come without Windows' own ding, so nothing plays twice.
//!
//! The tones are synthesized (soft bell-like notes with a quick attack and a
//! natural decay), so there are no audio files, and played from memory with
//! Windows' PlaySound.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// One of Monaged's sounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cue {
    /// An agent asks permission or a question, wants a secret, or a plan or
    /// decision waits for you.
    NeedsYou,
    /// An agent finished its turn while you weren't watching it.
    Done,
    /// A pipeline finished its feature; the setup guide is done.
    Complete,
    /// Something long you started finished: an install, a clone, a merge or push, a sign-in, an update.
    Success,
    /// A session failed, the connection to Claude dropped, or something long you started failed.
    Error,
}

impl Cue {
    pub const ALL: [Cue; 5] = [Cue::NeedsYou, Cue::Done, Cue::Complete, Cue::Success, Cue::Error];

    pub fn key(self) -> &'static str {
        match self {
            Cue::NeedsYou => "needs-you",
            Cue::Done => "done",
            Cue::Complete => "complete",
            Cue::Success => "success",
            Cue::Error => "error",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Cue::NeedsYou => "Needs you",
            Cue::Done => "An agent is done",
            Cue::Complete => "A feature is complete",
            Cue::Success => "Finished",
            Cue::Error => "Something went wrong",
        }
    }

    /// When it plays, in a sentence.
    pub fn when(self) -> &'static str {
        match self {
            Cue::NeedsYou => "An agent asks permission or a question, or a plan or decision waits for you.",
            Cue::Done => "An agent finished its turn while you were looking at something else.",
            Cue::Complete => "A pipeline merged its feature together, or setting up is done.",
            Cue::Success => "Something you started that takes a while worked: an install, a clone, a merge or push, a sign-in, an update.",
            Cue::Error => "A session failed, the connection to Claude dropped, or something you started failed.",
        }
    }

    /// Which wins when two come close together (higher wins).
    fn priority(self) -> u8 {
        match self {
            Cue::NeedsYou => 4,
            Cue::Error => 3,
            Cue::Complete => 2,
            Cue::Done => 1,
            Cue::Success => 0,
        }
    }

    /// The loudness order: alerts loudest, everyday feedback quietest.
    fn level(self) -> f32 {
        match self {
            Cue::NeedsYou => 1.0,
            Cue::Error => 0.85,
            Cue::Complete => 0.8,
            Cue::Done => 0.68,
            Cue::Success => 0.48,
        }
    }

    /// The notes: (start in seconds, frequency in Hz, relative gain), and how
    /// long each rings (its decay time constant, in seconds).
    fn notes(self) -> (&'static [(f32, f32, f32)], f32) {
        // Equal temperament, A4 = 440 Hz.
        const C4: f32 = 261.63;
        const E4: f32 = 329.63;
        const C5: f32 = 523.25;
        const E5: f32 = 659.25;
        const G5: f32 = 783.99;
        const A5: f32 = 880.00;
        const C6: f32 = 1046.50;
        const D6: f32 = 1174.66;
        const E6: f32 = 1318.51;
        match self {
            // Two quick, bright notes a fifth apart: "ding-ding", asking.
            Cue::NeedsYou => (&[(0.0, A5, 0.8), (0.11, E6, 1.0)], 0.16),
            // A soft rising major third: done, nothing to worry about.
            Cue::Done => (&[(0.0, C5, 0.85), (0.13, E5, 1.0)], 0.26),
            // A rising arpeggio to the octave: the rare, celebratory one.
            Cue::Complete => (&[(0.0, C5, 0.7), (0.085, E5, 0.75), (0.17, G5, 0.8), (0.255, C6, 1.0)], 0.32),
            // A short, quiet lift: it worked.
            Cue::Success => (&[(0.0, G5, 0.7), (0.06, D6, 1.0)], 0.1),
            // A low, falling major third: something needs a look.
            Cue::Error => (&[(0.0, E4, 1.0), (0.16, C4, 0.95)], 0.24),
        }
    }

    /// The timbre: partials as (multiple of the note, gain, how fast it
    /// fades relative to the fundamental). Brighter for alerts, warmer for
    /// feedback, darker for errors.
    fn partials(self) -> &'static [(f32, f32, f32)] {
        match self {
            Cue::NeedsYou => &[(1.0, 1.0, 1.0), (2.0, 0.32, 0.6), (3.0, 0.12, 0.4), (4.2, 0.05, 0.25)],
            Cue::Complete => &[(1.0, 1.0, 1.0), (2.0, 0.28, 0.6), (3.0, 0.1, 0.4), (5.4, 0.035, 0.2)],
            Cue::Done | Cue::Success => &[(1.0, 1.0, 1.0), (2.0, 0.16, 0.5), (3.0, 0.05, 0.3)],
            // Low notes: a stronger second partial carries them on laptop
            // speakers, which barely play 260–330 Hz (the ear hears the pitch anyway).
            Cue::Error => &[(1.0, 1.0, 1.0), (2.0, 0.3, 0.55), (3.0, 0.06, 0.3)],
        }
    }
}

/// How loud the cues are overall.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Volume {
    Quiet,
    Normal,
    Loud,
}

impl Volume {
    pub const ALL: [Volume; 3] = [Volume::Quiet, Volume::Normal, Volume::Loud];

    pub fn key(self) -> &'static str {
        match self {
            Volume::Quiet => "quiet",
            Volume::Normal => "normal",
            Volume::Loud => "loud",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Volume::Quiet => "Quiet",
            Volume::Normal => "Normal",
            Volume::Loud => "Loud",
        }
    }

    fn gain(self) -> f32 {
        match self {
            Volume::Quiet => 0.3,
            Volume::Normal => 0.55,
            Volume::Loud => 0.9,
        }
    }

    fn from_key(key: &str) -> Option<Volume> {
        Volume::ALL.into_iter().find(|v| v.key() == key)
    }
}

// ---- Making the sounds -------------------------------------------------------------------------

pub const SAMPLE_RATE: u32 = 44_100;

/// A cue as 16-bit mono samples at `SAMPLE_RATE`, its loudest point at the
/// cue's level times `gain` (0–1).
pub fn render(cue: Cue, gain: f32) -> Vec<i16> {
    let (notes, decay) = cue.notes();
    let partials = cue.partials();
    let rate = SAMPLE_RATE as f32;
    // Long enough for the last note to fade to silence (about -50 dB).
    let length = notes.iter().map(|(at, _, _)| at).fold(0.0f32, |a, b| a.max(*b)) + decay * 6.0 + 0.02;
    let mut out = vec![0f32; (length * rate) as usize];
    for &(at, freq, note_gain) in notes {
        let start = (at * rate) as usize;
        for (i, sample) in out.iter_mut().enumerate().skip(start) {
            let t = (i - start) as f32 / rate;
            // A 5 ms rise keeps the start from clicking.
            let attack = (t / 0.005).min(1.0);
            let mut value = 0.0;
            for &(ratio, partial_gain, fade) in partials {
                value += partial_gain * (-t / (decay * fade)).exp() * (std::f32::consts::TAU * freq * ratio * t).sin();
            }
            *sample += note_gain * attack * value;
        }
    }
    // A short fade at the very end, then scale to the cue's level.
    let tail = (0.015 * rate) as usize;
    let len = out.len();
    for (i, sample) in out.iter_mut().enumerate().skip(len.saturating_sub(tail)) {
        *sample *= (len - 1 - i) as f32 / tail as f32;
    }
    let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs())).max(1e-6);
    let scale = cue.level() * gain.clamp(0.0, 1.0) * 0.9 / peak;
    out.iter().map(|s| (s * scale * i16::MAX as f32).round() as i16).collect()
}

/// Samples as a WAV file in memory (16-bit PCM, mono), as PlaySound wants it.
pub fn wav(samples: &[i16]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes()); // bytes per second
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

// ---- When to play --------------------------------------------------------------------------------

/// Two sounds closer together than this are one moment: only the more important plays.
const TOGETHER: Duration = Duration::from_millis(1500);
/// The same sound again this soon is left out (several agents finishing one after another).
const AGAIN: Duration = Duration::from_secs(5);

/// Whether `cue` may play `now`, given the last sound and when this one last played.
pub fn allowed(last: Option<(Instant, Cue)>, last_same: Option<Instant>, cue: Cue, now: Instant) -> bool {
    if last_same.is_some_and(|at| now.duration_since(at) < AGAIN) {
        return false;
    }
    match last {
        Some((at, previous)) if now.duration_since(at) < TOGETHER => cue.priority() > previous.priority(),
        _ => true,
    }
}

/// What you chose in Settings → Sounds.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub on: bool,
    pub volume: Volume,
    /// Cues you turned off.
    pub off: HashSet<Cue>,
    /// Play "needs you" and "done" even for the agent you're looking at.
    pub while_watching: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { on: true, volume: Volume::Normal, off: HashSet::new(), while_watching: false }
    }
}

impl Settings {
    pub fn load(store: &crate::store::Store) -> Settings {
        let flag = |key: &str, default: bool| store.setting(key).map_or(default, |v| v == "1");
        Settings {
            on: flag("sound:on", true),
            volume: store.setting("sound:volume").and_then(|v| Volume::from_key(&v)).unwrap_or(Volume::Normal),
            off: Cue::ALL.into_iter().filter(|c| !flag(&format!("sound:cue:{}", c.key()), true)).collect(),
            while_watching: flag("sound:watching", false),
        }
    }

    pub fn save(&self, store: &crate::store::Store) {
        let bit = |on: bool| if on { "1" } else { "0" };
        let _ = store.set_setting("sound:on", bit(self.on));
        let _ = store.set_setting("sound:volume", self.volume.key());
        let _ = store.set_setting("sound:watching", bit(self.while_watching));
        for cue in Cue::ALL {
            let _ = store.set_setting(&format!("sound:cue:{}", cue.key()), bit(!self.off.contains(&cue)));
        }
    }

    pub fn plays(&self, cue: Cue) -> bool {
        self.on && !self.off.contains(&cue)
    }
}

/// Plays the cues by the rules above; one per Monaged window.
#[derive(Default)]
pub struct Player {
    pub settings: Settings,
    last: Option<(Instant, Cue)>,
    last_by_cue: HashMap<Cue, Instant>,
}

impl Player {
    pub fn new(settings: Settings) -> Player {
        let player = Player { settings, ..Player::default() };
        player.apply();
        player
    }

    /// Keeps Windows' own notification ding off while Monaged's sounds are on.
    pub fn apply(&self) {
        crate::notify::set_silent(self.settings.on && self.settings.plays(Cue::NeedsYou));
    }

    /// Plays `cue` if the settings, Windows and the rules allow it. `focused`:
    /// Monaged's window is the active one (it may then play even in states
    /// Windows calls busy, like a full-screen Monaged).
    pub fn play(&mut self, cue: Cue, focused: bool) -> bool {
        if !self.settings.plays(cue) || !windows_allows(focused) {
            return false;
        }
        let now = Instant::now();
        if !allowed(self.last, self.last_by_cue.get(&cue).copied(), cue, now) {
            return false;
        }
        self.last = Some((now, cue));
        self.last_by_cue.insert(cue, now);
        output::play(cue, self.settings.volume);
        true
    }

    /// Plays `cue` now whatever the rules say (the preview buttons in Settings).
    pub fn preview(&mut self, cue: Cue) {
        output::play(cue, self.settings.volume);
    }
}

/// Windows says it's a good moment: not presenting, no full-screen app (other
/// than Monaged), notifications not held back, someone at the PC.
fn windows_allows(focused: bool) -> bool {
    match output::notification_state() {
        // Away (locked or screen saver) or presenting: never.
        Some(1) | Some(4) => false,
        // A full-screen app or Direct3D game: unless it's Monaged itself.
        Some(2) | Some(3) => focused,
        // The first hour after signing in to a new PC (quiet time): only when you're here.
        Some(6) => focused,
        _ => true,
    }
}

#[cfg(windows)]
mod output {
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};

    use super::{Cue, Volume};

    #[link(name = "winmm")]
    unsafe extern "system" {
        fn PlaySoundW(sound: *const u16, module: isize, flags: u32) -> i32;
    }
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn SHQueryUserNotificationState(state: *mut i32) -> i32;
    }
    const SND_ASYNC: u32 = 0x0001;
    const SND_NODEFAULT: u32 = 0x0002;
    const SND_MEMORY: u32 = 0x0004;

    /// The rendered sounds. PlaySound reads them while they play (after it
    /// returns), so they live as long as Monaged: at most 15 small files.
    type Rendered = HashMap<(Cue, Volume), &'static [u8]>;
    static CACHE: LazyLock<Mutex<Rendered>> = LazyLock::new(|| Mutex::new(HashMap::new()));

    pub fn play(cue: Cue, volume: Volume) {
        let Ok(mut cache) = CACHE.lock() else { return };
        let bytes = *cache.entry((cue, volume)).or_insert_with(|| Box::leak(super::wav(&super::render(cue, volume.gain())).into_boxed_slice()));
        // SAFETY: `bytes` is a complete WAV file that is never freed; with
        // SND_MEMORY|SND_ASYNC PlaySound plays it from there and returns at once.
        unsafe {
            PlaySoundW(bytes.as_ptr().cast(), 0, SND_MEMORY | SND_ASYNC | SND_NODEFAULT);
        }
    }

    pub fn notification_state() -> Option<i32> {
        let mut state = 0;
        // SAFETY: one out parameter.
        let result = unsafe { SHQueryUserNotificationState(&mut state) };
        (result >= 0).then_some(state)
    }
}

#[cfg(not(windows))]
mod output {
    use super::{Cue, Volume};

    pub fn play(_: Cue, _: Volume) {}

    pub fn notification_state() -> Option<i32> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_cue_renders_short_clean_and_at_its_level() {
        for cue in Cue::ALL {
            let samples = render(cue, 1.0);
            let seconds = samples.len() as f32 / SAMPLE_RATE as f32;
            assert!((0.2..2.6).contains(&seconds), "{cue:?} lasts {seconds}s");
            let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap() as f32 / i16::MAX as f32;
            assert!((peak - cue.level() * 0.9).abs() < 0.02, "{cue:?} peaks at {peak}");
            // Starts and ends at silence, so it doesn't click.
            assert!(samples[0].unsigned_abs() < 200, "{cue:?} starts at {}", samples[0]);
            assert_eq!(*samples.last().unwrap(), 0, "{cue:?}");
        }
    }

    #[test]
    fn the_loudness_order_holds_and_volume_scales() {
        let peak = |cue, gain| render(cue, gain).iter().map(|s| s.unsigned_abs()).max().unwrap();
        assert!(peak(Cue::NeedsYou, 1.0) > peak(Cue::Error, 1.0));
        assert!(peak(Cue::Error, 1.0) > peak(Cue::Done, 1.0));
        assert!(peak(Cue::Done, 1.0) > peak(Cue::Success, 1.0));
        assert!(peak(Cue::Done, Volume::Quiet.gain()) < peak(Cue::Done, Volume::Loud.gain()));
    }

    #[test]
    fn good_news_rises_and_trouble_falls() {
        for cue in [Cue::NeedsYou, Cue::Done, Cue::Complete, Cue::Success] {
            let (notes, _) = cue.notes();
            assert!(notes.windows(2).all(|w| w[1].1 > w[0].1), "{cue:?} should rise");
        }
        let (notes, _) = Cue::Error.notes();
        assert!(notes.windows(2).all(|w| w[1].1 < w[0].1), "errors fall");
    }

    #[test]
    fn makes_a_valid_wav() {
        let samples = render(Cue::Success, 0.5);
        let bytes = wav(&samples);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..16], b"WAVEfmt ");
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize, bytes.len() - 8);
        assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), SAMPLE_RATE);
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()) as usize, samples.len() * 2);
        assert_eq!(bytes.len(), 44 + samples.len() * 2);
    }

    #[test]
    fn bursts_become_one_sound_and_the_important_one_wins() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        // Nothing played yet: anything goes.
        assert!(allowed(None, None, Cue::Success, t0));
        // Right after "done", a second "done" is the same moment.
        assert!(!allowed(Some((t0, Cue::Done)), Some(t0), Cue::Done, at(300)));
        // ...and a less important cue too, but "needs you" still plays.
        assert!(!allowed(Some((t0, Cue::Done)), None, Cue::Success, at(300)));
        assert!(allowed(Some((t0, Cue::Done)), None, Cue::NeedsYou, at(300)));
        // Later, other cues play again; the same cue waits a little longer.
        assert!(allowed(Some((t0, Cue::Done)), None, Cue::Success, at(2000)));
        assert!(!allowed(Some((t0, Cue::Done)), Some(t0), Cue::Done, at(2000)));
        assert!(allowed(Some((t0, Cue::Done)), Some(t0), Cue::Done, at(6000)));
    }

    /// `MONAGED_SOUND_DIR=<folder> cargo test write_the_cues` writes every cue
    /// (at Normal volume) as a WAV file there, to listen to them outside Monaged.
    #[test]
    fn write_the_cues() {
        let Some(dir) = std::env::var_os("MONAGED_SOUND_DIR") else { return };
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        for cue in Cue::ALL {
            std::fs::write(dir.join(format!("{}.wav", cue.key())), wav(&render(cue, Volume::Normal.gain()))).unwrap();
        }
    }

    #[test]
    fn settings_say_what_plays() {
        let mut settings = Settings::default();
        assert!(Cue::ALL.iter().all(|c| settings.plays(*c)));
        settings.off.insert(Cue::Success);
        assert!(!settings.plays(Cue::Success) && settings.plays(Cue::Done));
        settings.on = false;
        assert!(!settings.plays(Cue::NeedsYou));
        assert_eq!(Volume::from_key("loud"), Some(Volume::Loud));
        assert_eq!(Volume::from_key("nope"), None);
    }
}
