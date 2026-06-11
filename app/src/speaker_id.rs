//! Deployment-local speaker identification for retained voice messages.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use std::{
    cmp::{Ordering, Reverse},
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use crabgent_channel::{
    SpeakerIdentificationError, SpeakerIdentificationRequest, SpeakerIdentifier,
};
use crabgent_core::{AudioPayload, SpeakerIdentity, SttResponse};
use crabgent_log::{info, warn};
use uuid::Uuid;

use crate::config::{SpeakerIdTomlConfig, SpeakerProfileTomlConfig};

const SAMPLE_RATE_HZ: usize = 16_000;
const FRAME_SAMPLES: usize = 400;
const HOP_SAMPLES: usize = 160;
const MIN_PITCH_HZ: usize = 70;
const MAX_PITCH_HZ: usize = 400;
const LOW_ENERGY_FLOOR: f64 = 0.006;
const AUDIO_EXTENSIONS: &[&str] = &["aac", "flac", "m4a", "mp3", "ogg", "opus", "wav", "webm"];

#[derive(Debug, Clone)]
pub struct LocalSpeakerIdentifier {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    ffmpeg_path: PathBuf,
    threshold: u8,
    margin: u8,
    profiles: Vec<SpeakerProfile>,
}

#[derive(Debug, Clone)]
struct SpeakerProfile {
    id: String,
    display: Option<String>,
    fingerprint: Voiceprint,
}

#[derive(Debug, Clone, Copy, Default)]
struct Voiceprint {
    pitch_hz: f64,
    zcr: f64,
    rms: f64,
    voiced_ratio: f64,
}

#[derive(Debug)]
struct Match {
    profile: SpeakerProfile,
    confidence: u8,
}

impl LocalSpeakerIdentifier {
    pub fn from_config(cfg: &SpeakerIdTomlConfig) -> Option<Arc<dyn SpeakerIdentifier>> {
        if !cfg.enabled {
            return None;
        }
        let ffmpeg_path = cfg
            .ffmpeg_path
            .clone()
            .unwrap_or_else(|| PathBuf::from("ffmpeg"));
        let mut profiles = Vec::new();
        for profile in &cfg.profiles {
            match load_profile(profile, &ffmpeg_path) {
                Ok(Some(profile)) => profiles.push(profile),
                Ok(None) => {}
                Err(err) => warn!(
                    speaker_profile = %profile.id,
                    error = %err,
                    "speaker profile could not be loaded"
                ),
            }
        }
        if profiles.is_empty() {
            warn!("speaker identification enabled but no usable profiles were found");
            return None;
        }
        info!(
            profile_count = profiles.len(),
            threshold = cfg.threshold.min(100),
            margin = cfg.margin.min(100),
            "speaker identification loaded"
        );
        Some(Arc::new(Self {
            inner: Arc::new(Inner {
                ffmpeg_path,
                threshold: cfg.threshold.min(100),
                margin: cfg.margin.min(100),
                profiles,
            }),
        }))
    }
}

#[async_trait]
impl SpeakerIdentifier for LocalSpeakerIdentifier {
    async fn identify(
        &self,
        req: SpeakerIdentificationRequest,
    ) -> Result<Vec<SpeakerIdentity>, SpeakerIdentificationError> {
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || inner.identify(&req))
            .await
            .map_err(|err| SpeakerIdentificationError::Backend(err.to_string()))?
            .map_err(|err| SpeakerIdentificationError::Backend(err.to_string()))
    }
}

impl Inner {
    fn identify(&self, req: &SpeakerIdentificationRequest) -> Result<Vec<SpeakerIdentity>> {
        let samples = decode_payload(&self.ffmpeg_path, &req.payload)?;
        let Some(input) = voiceprint(&samples) else {
            return Ok(Vec::new());
        };
        let mut matches = self
            .profiles
            .iter()
            .cloned()
            .map(|profile| Match {
                confidence: confidence(input, profile.fingerprint),
                profile,
            })
            .collect::<Vec<_>>();
        matches.sort_unstable_by_key(|entry| Reverse(entry.confidence));
        let Some(best) = matches.first() else {
            return Ok(Vec::new());
        };
        let runner_up = matches.get(1).map_or(0, |m| m.confidence);
        if best.confidence < self.threshold
            || best.confidence.saturating_sub(runner_up) < self.margin
        {
            info!(
                best_profile = %best.profile.id,
                confidence = best.confidence,
                runner_up,
                threshold = self.threshold,
                margin = self.margin,
                "speaker identification rejected"
            );
            return Ok(Vec::new());
        }

        info!(
            speaker_profile = %best.profile.id,
            confidence = best.confidence,
            runner_up,
            "speaker identification matched"
        );
        let mut identity =
            SpeakerIdentity::new(&best.profile.id, "local-voiceprint", best.confidence);
        if let Some(display) = &best.profile.display {
            identity = identity.with_display(display);
        }
        if let Some(label) = single_stt_speaker_label(&req.transcription) {
            identity = identity.with_speaker_label(label);
        }
        Ok(vec![identity])
    }
}

fn load_profile(
    cfg: &SpeakerProfileTomlConfig,
    ffmpeg_path: &Path,
) -> Result<Option<SpeakerProfile>> {
    let paths = collect_sample_paths(&cfg.samples)?;
    if paths.is_empty() {
        warn!(speaker_profile = %cfg.id, "speaker profile has no readable audio samples");
        return Ok(None);
    }
    let mut prints = Vec::new();
    for path in paths {
        match decode_file(ffmpeg_path, &path).and_then(|samples| {
            voiceprint(&samples).ok_or_else(|| anyhow!("sample contains no usable speech"))
        }) {
            Ok(print) => prints.push(print),
            Err(err) => warn!(
                speaker_profile = %cfg.id,
                path = %path.display(),
                error = %err,
                "speaker sample skipped"
            ),
        }
    }
    if prints.is_empty() {
        warn!(speaker_profile = %cfg.id, "speaker profile has no usable voiceprint samples");
        return Ok(None);
    }
    info!(
        speaker_profile = %cfg.id,
        sample_count = prints.len(),
        "speaker profile loaded"
    );
    Ok(Some(SpeakerProfile {
        id: cfg.id.trim().to_owned(),
        display: cfg.display.as_ref().map(|value| value.trim().to_owned()),
        fingerprint: average_voiceprints(&prints),
    }))
}

fn collect_sample_paths(roots: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for root in roots {
        let root = expand_home(root);
        if root.is_file() {
            if is_audio_file(&root) {
                out.push(root);
            }
            continue;
        }
        if root.is_dir() {
            collect_dir(&root, &mut out)?;
            continue;
        }
        warn!(path = %root.display(), "speaker sample path does not exist");
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

fn collect_dir(root: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).with_context(|| format!("read {}", dir.display()))? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file() && is_audio_file(&path) {
                out.push(path);
            }
        }
    }
    Ok(())
}

fn expand_home(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if text == "~" {
        return env::var_os("HOME").map_or_else(|| path.to_path_buf(), PathBuf::from);
    }
    if let Some(rest) = text.strip_prefix("~/")
        && let Some(home) = env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    path.to_path_buf()
}

fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| AUDIO_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
}

fn decode_payload(ffmpeg_path: &Path, payload: &AudioPayload) -> Result<Vec<i16>> {
    let ext = payload_extension(payload);
    let path = env::temp_dir().join(format!("crabgent-speaker-{}.{}", Uuid::new_v4(), ext));
    fs::write(&path, payload.bytes()).with_context(|| format!("write {}", path.display()))?;
    let result = decode_file(ffmpeg_path, &path);
    let _ = fs::remove_file(&path);
    result
}

fn payload_extension(payload: &AudioPayload) -> &'static str {
    let Some((kind, subtype)) = payload.mime().split_once('/') else {
        return "bin";
    };
    if kind != "audio" && kind != "video" {
        return "bin";
    }
    match subtype.split(';').next().unwrap_or_default() {
        "mpeg" => "mp3",
        "mp4" | "x-m4a" => "m4a",
        "ogg" | "opus" => "ogg",
        "wav" | "wave" | "x-wav" => "wav",
        "webm" => "webm",
        _ => "bin",
    }
}

fn decode_file(ffmpeg_path: &Path, path: &Path) -> Result<Vec<i16>> {
    let output = Command::new(ffmpeg_path)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            path.to_str()
                .ok_or_else(|| anyhow!("non-utf8 audio path {}", path.display()))?,
            "-ac",
            "1",
            "-ar",
            "16000",
            "-f",
            "s16le",
            "pipe:1",
        ])
        .output()
        .with_context(|| format!("run {}", ffmpeg_path.display()))?;
    if !output.status.success() {
        bail!(
            "ffmpeg failed for {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output
        .stdout
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect())
}

fn voiceprint(samples: &[i16]) -> Option<Voiceprint> {
    if samples.len() < FRAME_SAMPLES {
        return None;
    }
    let global_rms = rms(samples);
    let energy_floor = (global_rms * 0.35).max(LOW_ENERGY_FLOOR);
    let mut pitches = Vec::new();
    let mut zcrs = Vec::new();
    let mut rms_values = Vec::new();
    let mut frame_count = 0_u32;
    let mut voiced_count = 0_u32;
    let mut offset = 0;
    while offset + FRAME_SAMPLES <= samples.len() {
        frame_count += 1;
        let frame = &samples[offset..offset + FRAME_SAMPLES];
        let frame_rms = rms(frame);
        if frame_rms >= energy_floor {
            voiced_count += 1;
            rms_values.push(frame_rms);
            zcrs.push(zero_crossing_rate(frame));
            if let Some(pitch) = pitch_hz(frame) {
                pitches.push(pitch);
            }
        }
        offset += HOP_SAMPLES;
    }
    if rms_values.is_empty() {
        return None;
    }
    Some(Voiceprint {
        pitch_hz: median(&mut pitches).unwrap_or_default(),
        zcr: mean(&zcrs),
        rms: mean(&rms_values),
        voiced_ratio: f64::from(voiced_count) / f64::from(frame_count.max(1)),
    })
}

fn rms(samples: &[i16]) -> f64 {
    let sum = samples
        .iter()
        .map(|sample| {
            let normalized = f64::from(*sample) / f64::from(i16::MAX);
            normalized * normalized
        })
        .sum::<f64>();
    (sum / samples.len().max(1) as f64).sqrt()
}

fn zero_crossing_rate(samples: &[i16]) -> f64 {
    if samples.len() < 2 {
        return 0.0;
    }
    let crossings = samples
        .windows(2)
        .filter(|pair| (pair[0] < 0 && pair[1] >= 0) || (pair[0] >= 0 && pair[1] < 0))
        .count();
    crossings as f64 / (samples.len() - 1) as f64
}

fn pitch_hz(frame: &[i16]) -> Option<f64> {
    let min_lag = SAMPLE_RATE_HZ / MAX_PITCH_HZ;
    let max_lag = SAMPLE_RATE_HZ / MIN_PITCH_HZ;
    let mean = frame.iter().map(|sample| f64::from(*sample)).sum::<f64>() / frame.len() as f64;
    let energy = frame
        .iter()
        .map(|sample| {
            let centered = f64::from(*sample) - mean;
            centered * centered
        })
        .sum::<f64>();
    if energy <= f64::EPSILON {
        return None;
    }
    let mut best_lag = 0;
    let mut best_corr = 0.0;
    for lag in min_lag..=max_lag.min(frame.len().saturating_sub(1)) {
        let corr = frame
            .iter()
            .zip(frame.iter().skip(lag))
            .map(|(left, right)| (f64::from(*left) - mean) * (f64::from(*right) - mean))
            .sum::<f64>()
            / energy;
        if corr > best_corr {
            best_corr = corr;
            best_lag = lag;
        }
    }
    (best_corr >= 0.30 && best_lag > 0).then_some(SAMPLE_RATE_HZ as f64 / best_lag as f64)
}

fn average_voiceprints(prints: &[Voiceprint]) -> Voiceprint {
    Voiceprint {
        pitch_hz: mean_by(prints, |p| p.pitch_hz),
        zcr: mean_by(prints, |p| p.zcr),
        rms: mean_by(prints, |p| p.rms),
        voiced_ratio: mean_by(prints, |p| p.voiced_ratio),
    }
}

fn confidence(input: Voiceprint, profile: Voiceprint) -> u8 {
    let pitch = if input.pitch_hz > 0.0 && profile.pitch_hz > 0.0 {
        (input.pitch_hz.ln() - profile.pitch_hz.ln()).abs() / 0.28
    } else {
        1.8
    };
    let zcr = (input.zcr - profile.zcr).abs() / 0.07;
    let rms = (input.rms - profile.rms).abs() / 0.10;
    let voiced = (input.voiced_ratio - profile.voiced_ratio).abs() / 0.30;
    let distance = (pitch.mul_add(1.8, zcr * 0.8) + rms * 0.5 + voiced).max(0.0);
    let score = 100.0 * (-0.55 * distance).exp();
    score.round().clamp(0.0, 100.0) as u8
}

fn single_stt_speaker_label(response: &SttResponse) -> Option<String> {
    let mut labels = response
        .segments
        .iter()
        .filter_map(|segment| segment.speaker_id.as_deref())
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .collect::<Vec<_>>();
    labels.sort_unstable();
    labels.dedup();
    (labels.len() == 1).then(|| labels[0].to_owned())
}

fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<f64>() / values.len() as f64
}

fn mean_by<F>(prints: &[Voiceprint], f: F) -> f64
where
    F: Fn(Voiceprint) -> f64,
{
    prints.iter().copied().map(f).sum::<f64>() / prints.len().max(1) as f64
}

fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable_by(|left, right| left.partial_cmp(right).unwrap_or(Ordering::Equal));
    Some(values[values.len() / 2])
}

#[cfg(test)]
mod tests {
    use super::{Voiceprint, confidence, expand_home, is_audio_file, voiceprint};
    use std::path::Path;

    #[test]
    fn audio_extension_filter_is_case_insensitive() {
        assert!(is_audio_file(Path::new("sample.WEBM")));
        assert!(!is_audio_file(Path::new("sample.txt")));
    }

    #[test]
    fn home_expansion_leaves_absolute_paths_alone() {
        assert_eq!(
            expand_home(Path::new("/tmp/audio.wav")),
            Path::new("/tmp/audio.wav")
        );
    }

    #[test]
    fn confidence_prefers_matching_prints() {
        let base = Voiceprint {
            pitch_hz: 140.0,
            zcr: 0.06,
            rms: 0.08,
            voiced_ratio: 0.65,
        };
        let close = Voiceprint {
            pitch_hz: 144.0,
            ..base
        };
        let far = Voiceprint {
            pitch_hz: 260.0,
            zcr: 0.14,
            rms: 0.02,
            voiced_ratio: 0.25,
        };

        assert!(confidence(base, close) > confidence(base, far));
    }

    #[test]
    fn voiceprint_rejects_short_audio() {
        assert!(voiceprint(&[0; 100]).is_none());
    }
}
