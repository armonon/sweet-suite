//! # suite-timeline — the generic track/keyframe/playhead engine.
//!
//! **One spine, three faces** (animation / NLE / beat-grid). Build the machine once —
//! its data model, its editor, its evaluation contract — and let each app supply only
//! the item types and the time base. The shared playhead/transport is what makes the
//! timeline *feel* identical across the suite. docs/03 §4.1–4.3.
//!
//! **This build implements the animation face** (Tier 1: "one timeline animates
//! everything"). Position/rotation/scale keyframe tracks per object, linear
//! interpolation, looping playhead, `K` to set a keyframe. The NLE and DAW faces share
//! this engine but supply different `TimeBase` and item types.

use std::collections::HashMap;
use serde::{Deserialize, Serialize};

/// The time axis — the only thing that differs between the three faces. docs/03 §4.1.
#[derive(Clone, Serialize, Deserialize)]
pub enum TimeBase {
    Seconds,
    Frames { fps: (u32, u32) },
    Musical,
}

/// Per-keyframe interpolation. `Constant` (stepped) is what makes the *same* engine do
/// frame-by-frame 2D animation; rotations interpolate as quaternion slerp. docs/03 §4.2.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Interp {
    Constant,
    Linear,
}

/// A scalar keyframe: time in seconds + value.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Key {
    pub time: f32,
    pub value: f32,
    pub interp: Interp,
}

/// A track of scalar keyframes, sorted by time.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Track {
    pub keys: Vec<Key>,
}

impl Track {
    /// Sample the track at time `t` (seconds). Returns `None` if the track is empty.
    pub fn sample(&self, t: f32) -> Option<f32> {
        if self.keys.is_empty() {
            return None;
        }
        // Before first key: hold first value.
        if t <= self.keys[0].time {
            return Some(self.keys[0].value);
        }
        // After last key: hold last value.
        if t >= self.keys.last().unwrap().time {
            return Some(self.keys.last().unwrap().value);
        }
        // Find the segment.
        let idx = self.keys.partition_point(|k| k.time <= t).saturating_sub(1);
        let a = &self.keys[idx];
        let b = &self.keys[idx + 1];
        let dt = b.time - a.time;
        if dt < 1e-8 {
            return Some(a.value);
        }
        let frac = (t - a.time) / dt;
        Some(match a.interp {
            Interp::Constant => a.value,
            Interp::Linear => a.value + (b.value - a.value) * frac,
        })
    }

    /// Insert or overwrite a keyframe at the given time.
    pub fn set_key(&mut self, time: f32, value: f32, interp: Interp) {
        // Replace if same time (within 0.5 ms tolerance).
        for k in &mut self.keys {
            if (k.time - time).abs() < 5e-4 {
                k.value = value;
                k.interp = interp;
                return;
            }
        }
        self.keys.push(Key { time, value, interp });
        self.keys.sort_by(|a, b| a.time.partial_cmp(&b.time).unwrap());
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

/// Transform tracks for a single object: 9 scalar tracks (tx, ty, tz, rx, ry, rz, sx, sy, sz).
/// Tracks with no keys are omitted from the sample (the object uses its document value).
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct ObjectTracks {
    pub tx: Track, pub ty: Track, pub tz: Track,
    pub rx: Track, pub ry: Track, pub rz: Track,
    pub sx: Track, pub sy: Track, pub sz: Track,
}

impl ObjectTracks {
    /// Sample all channels at `t`. Missing channels return `None`.
    pub fn sample(&self, t: f32) -> SampledTransform {
        SampledTransform {
            tx: self.tx.sample(t), ty: self.ty.sample(t), tz: self.tz.sample(t),
            rx: self.rx.sample(t), ry: self.ry.sample(t), rz: self.rz.sample(t),
            sx: self.sx.sample(t), sy: self.sy.sample(t), sz: self.sz.sample(t),
        }
    }
}

/// Result of sampling `ObjectTracks` — each field is `Some(value)` if a track was set.
pub struct SampledTransform {
    pub tx: Option<f32>, pub ty: Option<f32>, pub tz: Option<f32>,
    pub rx: Option<f32>, pub ry: Option<f32>, pub rz: Option<f32>,
    pub sx: Option<f32>, pub sy: Option<f32>, pub sz: Option<f32>,
}

/// Three Euler-angle tracks (degrees, XYZ) for a single bone's pose rotation.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct BoneTracks {
    pub rx: Track,
    pub ry: Track,
    pub rz: Track,
}

impl BoneTracks {
    /// Sample (rx, ry, rz) in degrees at `t`. Channels with no keys return 0.
    pub fn sample(&self, t: f32) -> [f32; 3] {
        [
            self.rx.sample(t).unwrap_or(0.0),
            self.ry.sample(t).unwrap_or(0.0),
            self.rz.sample(t).unwrap_or(0.0),
        ]
    }
    pub fn is_empty(&self) -> bool {
        self.rx.is_empty() && self.ry.is_empty() && self.rz.is_empty()
    }
}

/// An animation clip: a duration (seconds) and per-object tracks, keyed by object serial.
/// Uses u64 (the serial from ObjId) so the clip can be serialized without the full
/// generational-arena dep.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct AnimationClip {
    pub name: String,
    pub duration: f32,
    pub loop_clip: bool,
    /// Key = object serial (ObjId::serial).
    pub tracks: HashMap<u64, ObjectTracks>,
    /// Bone pose tracks. Outer key = object serial, inner key = bone index. Nested (rather
    /// than a `(u64,u32)` tuple key) so it round-trips through JSON, which can't use tuple
    /// map keys. `default` for back-compat with clips saved before rigging existed.
    #[serde(default)]
    pub bone_tracks: HashMap<u64, HashMap<u32, BoneTracks>>,
}

impl AnimationClip {
    pub fn new(name: impl Into<String>, duration: f32) -> Self {
        Self {
            name: name.into(),
            duration,
            loop_clip: true,
            tracks: HashMap::new(),
            bone_tracks: HashMap::new(),
        }
    }
}

/// Playhead + transport state, lives in the shell or the app.
#[derive(Clone, Serialize, Deserialize)]
pub struct Playhead {
    pub time: f32,
    pub playing: bool,
    pub clip_duration: f32,
    pub loop_play: bool,
}

impl Default for Playhead {
    fn default() -> Self {
        Self {
            time: 0.0,
            playing: false,
            clip_duration: 4.0,
            loop_play: true,
        }
    }
}

impl Playhead {
    /// Advance by `dt` seconds; wraps if looping, clamps otherwise.
    pub fn advance(&mut self, dt: f32) {
        if !self.playing {
            return;
        }
        self.time += dt;
        if self.loop_play {
            if self.clip_duration > 0.0 {
                self.time = self.time.rem_euclid(self.clip_duration);
            }
        } else {
            self.time = self.time.min(self.clip_duration);
            if self.time >= self.clip_duration {
                self.playing = false;
            }
        }
    }
}

/// The generic timeline: tracks of time-addressed items + a playhead.
#[derive(Clone, Serialize, Deserialize)]
pub struct Timeline {
    pub time_base: TimeBase,
    pub clip: AnimationClip,
    pub playhead: Playhead,
}

impl Default for Timeline {
    fn default() -> Self {
        Self {
            time_base: TimeBase::Seconds,
            clip: AnimationClip::new("untitled", 4.0),
            playhead: Playhead::default(),
        }
    }
}

impl Timeline {
    /// Set a keyframe for the given object (serial). Adds a keyframe for all 9 TRS channels.
    /// `pos: [x,y,z]`, `rot: [rx,ry,rz]` (Euler angles), `scale: [sx,sy,sz]`.
    pub fn set_keyframe_trs(
        &mut self,
        obj_serial: u64,
        pos: [f32; 3],
        rot: [f32; 3],
        scale: [f32; 3],
    ) {
        let t = self.playhead.time;
        let tracks = self.clip.tracks.entry(obj_serial).or_default();
        tracks.tx.set_key(t, pos[0], Interp::Linear);
        tracks.ty.set_key(t, pos[1], Interp::Linear);
        tracks.tz.set_key(t, pos[2], Interp::Linear);
        tracks.rx.set_key(t, rot[0], Interp::Linear);
        tracks.ry.set_key(t, rot[1], Interp::Linear);
        tracks.rz.set_key(t, rot[2], Interp::Linear);
        tracks.sx.set_key(t, scale[0], Interp::Linear);
        tracks.sy.set_key(t, scale[1], Interp::Linear);
        tracks.sz.set_key(t, scale[2], Interp::Linear);
    }

    /// Sample all object tracks at `playhead.time`. Caller applies the result to the document.
    pub fn sample_all(&self) -> HashMap<u64, SampledTransform> {
        let t = self.playhead.time;
        self.clip
            .tracks
            .iter()
            .map(|(&serial, tracks)| (serial, tracks.sample(t)))
            .collect()
    }

    /// Key a bone's Euler pose (degrees) at the current playhead time.
    pub fn set_bone_keyframe(&mut self, obj_serial: u64, bone: u32, euler_deg: [f32; 3]) {
        let t = self.playhead.time;
        let tracks = self
            .clip
            .bone_tracks
            .entry(obj_serial)
            .or_default()
            .entry(bone)
            .or_default();
        tracks.rx.set_key(t, euler_deg[0], Interp::Linear);
        tracks.ry.set_key(t, euler_deg[1], Interp::Linear);
        tracks.rz.set_key(t, euler_deg[2], Interp::Linear);
    }

    /// Sample all bone pose tracks at `playhead.time`. Returns `(obj_serial, bone) → euler_deg`.
    pub fn sample_bones(&self) -> Vec<(u64, u32, [f32; 3])> {
        let t = self.playhead.time;
        let mut out = Vec::new();
        for (&serial, bones) in &self.clip.bone_tracks {
            for (&bone, tracks) in bones {
                if !tracks.is_empty() {
                    out.push((serial, bone, tracks.sample(t)));
                }
            }
        }
        out
    }

    pub fn play(&mut self) {
        self.playhead.playing = true;
    }

    pub fn pause(&mut self) {
        self.playhead.playing = false;
    }

    pub fn stop(&mut self) {
        self.playhead.playing = false;
        self.playhead.time = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_linear_interp() {
        let mut t = Track::default();
        t.set_key(0.0, 0.0, Interp::Linear);
        t.set_key(1.0, 10.0, Interp::Linear);
        assert!((t.sample(0.5).unwrap() - 5.0).abs() < 1e-5);
        assert!((t.sample(0.0).unwrap() - 0.0).abs() < 1e-5);
        assert!((t.sample(1.0).unwrap() - 10.0).abs() < 1e-5);
        assert!((t.sample(2.0).unwrap() - 10.0).abs() < 1e-5);
    }

    #[test]
    fn track_constant_interp() {
        let mut t = Track::default();
        t.set_key(0.0, 5.0, Interp::Constant);
        t.set_key(1.0, 20.0, Interp::Constant);
        assert_eq!(t.sample(0.5).unwrap(), 5.0); // stepped, holds first
        assert_eq!(t.sample(1.5).unwrap(), 20.0);
    }

    #[test]
    fn playhead_loops() {
        let mut ph = Playhead { clip_duration: 4.0, loop_play: true, playing: true, ..Default::default() };
        ph.advance(3.9);
        ph.advance(0.2);
        assert!(ph.time < 0.5, "should wrap around");
    }

    #[test]
    fn timeline_bone_keyframe_interpolates() {
        let mut tl = Timeline::default();
        // Key bone 2 of object 7 at t=0 (neutral) and t=2 (90° about Z).
        tl.playhead.time = 0.0;
        tl.set_bone_keyframe(7, 2, [0.0, 0.0, 0.0]);
        tl.playhead.time = 2.0;
        tl.set_bone_keyframe(7, 2, [0.0, 0.0, 90.0]);
        // Sample at the midpoint.
        tl.playhead.time = 1.0;
        let samples = tl.sample_bones();
        let found = samples.iter().find(|(s, b, _)| *s == 7 && *b == 2).expect("bone track sampled");
        assert!((found.2[2] - 45.0).abs() < 1e-3, "z euler interpolates to 45° at midpoint, got {}", found.2[2]);
        assert!(found.2[0].abs() < 1e-3 && found.2[1].abs() < 1e-3, "x/y stay 0");
    }

    #[test]
    fn timeline_set_and_sample_keyframe() {
        let mut tl = Timeline::default();
        tl.playhead.time = 1.0;
        tl.set_keyframe_trs(42, [1.0, 2.0, 3.0], [0.0; 3], [1.0; 3]);
        tl.playhead.time = 0.0;
        tl.set_keyframe_trs(42, [0.0; 3], [0.0; 3], [1.0; 3]);
        tl.playhead.time = 0.5;
        let samples = tl.sample_all();
        let s = &samples[&42];
        assert!((s.tx.unwrap() - 0.5).abs() < 1e-4, "linear interp tx");
        assert!((s.ty.unwrap() - 1.0).abs() < 1e-4, "linear interp ty");
    }
}
