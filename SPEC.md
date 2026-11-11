# Heard — found sounds

GPL-3.0. See README for usage, CREDITS.md for components.

## Stack
- Rust 1.95 (rust-toolchain.toml), `eframe`/`egui` 0.36 on wgpu, `egui-snarl` 0.12 (node
  editor), `petgraph` (cycle checks), `symphonia` (decode), `hound` (WAV out), `cpal`
  (playback), `rustfft` (spectral ops, spectrogram), `rfd`, `serde`. Forked from Found; the
  shell, theme, vim layer, settings, undo, worker and graph code are shared in spirit.
- Tests: unit tests for the DSP and library, `egui_kittest` e2e for the core loop.

## Inputs
- WAV/BWF, AIFF, FLAC, MP3, M4A (AAC/ALAC), Ogg Vorbis. Typical material: 0.2 s to 10 min,
  44.1–96 kHz, mono or stereo. Decoded fully to planar f32 (a 3-minute stereo take decodes in
  about 50–120 ms). The editor works on the full sound, with a per-node result cache, so a
  slider only re-runs the nodes downstream of it. No proxy: the preview is the render.
- Import: files into the Inbox; folders, where sub-folders become collections and their
  sounds are filed as they are.

## Library on disk
- Folder per collection. A sound keeps its name: `name.<ext>` (original, never modified),
  `name.edit.wav` (rendered edit), `name.json` (recipe, tags, notes, source name). Untouched
  sounds have no edit. Renaming moves all three; clashes get `-2`.
- Inbox recipes in `.heard/inbox-edits/`, local trash in `.heard/trash/`, user presets in
  `_presets/`.

## Editing
- Graph: `Source → Trim → High-pass → Noise Reduction → Fade → Gain → Output`, all neutral
  by default (render = original).
- Presentation: waveform (min/max pyramid, sample view when zoomed in), ruler, selection,
  playhead, markers, optional log-frequency spectrogram; node cards with sliders. Patch: the
  same graph in egui-snarl with branches and a two-input Mix.
- Trim tool shows the trim node's input with draggable edges. Before/after shows and plays
  the original.
- Slicing: markers (manual or onset detection), then Split / Extract into new Inbox WAVs.
- Playback: cpal, resampled to the device rate on the fly; live buffer swap while tweaking.
  Silent fallback without a device.

## Nodes
- Clean up: Trim, Auto Trim, High-pass / Low-pass (Butterworth 12/24/48), EQ, Hum Removal,
  Noise Reduction (spectral gating with a learned or automatic profile, blockwise STFT),
  De-click, Gate, Compressor, Gain, Normalize (peak / LUFS / RMS), Fade, Channels, Loop.
- Experimental: Reverse, Varispeed, Paulstretch, Freeze, Pitch Shift, Granular, Stutter,
  Bit Crush, Tape, Ring Mod, Resonator, Echo, Reverb, Drive. Routing: Mix (2 in), Sidechain
  (2 in), Split (3 out, bands sum back exactly). Seeded and randomizable.
- Presets: clean-up chains and experimental patches; applying keeps the trim and the learned
  noise profile.

## Export
- WAV, source sample rate, channels as rendered, 24-bit int (32-bit float for float
  sources).

## Look
- Found's light-grey patcher theme: mid-grey chrome, dark object boxes, pastel node tints,
  a single blue accent. Waveforms are dark object-grey on a light lane. Library tiles are
  waveform panels above a dark caption box with ▶, name and length; the inlet bar turns
  green when an edit has been rendered.

## Not in v1
- An arrangement / timeline workspace (Found's Collage equivalent).
- De-clip, de-reverb, spectral repair (painting out single events), batch loudness reports.
