# Credits

Heard is free software under the [GNU GPL v3](LICENSE).

Built with [egui / eframe](https://github.com/emilk/egui) (with [wgpu](https://wgpu.rs)),
[egui-snarl](https://github.com/zakarumych/egui-snarl), [symphonia](https://github.com/pdeljanov/Symphonia)
(audio decoding), [cpal](https://github.com/RustAudio/cpal) (playback),
[hound](https://github.com/ruuda/hound) (WAV writing), [RustFFT](https://github.com/ejmahler/RustFFT),
[rfd](https://github.com/PolyMeilex/rfd), [rayon](https://github.com/rayon-rs/rayon) and
[petgraph](https://github.com/petgraph/petgraph), plus their dependencies — all under licences
compatible with the GPL v3 (MIT, Apache-2.0, BSD, ISC, Zlib, Unicode, MPL-2.0); `cargo tree`
lists them.

The symphonia crates and `option-ext` are MPL-2.0 and used unmodified; their source code is
available at <https://github.com/pdeljanov/Symphonia> and <https://crates.io/crates/option-ext>.

## Techniques

Heard's DSP is its own code; these published methods are what it implements:

- Filters and shelves: the RBJ *Audio EQ Cookbook* (Robert Bristow-Johnson).
- Loudness (LUFS): ITU-R BS.1770-4 K-weighting and gating.
- Noise reduction: spectral gating as described for Audacity's Noise Reduction and
  [noisereduce](https://github.com/timsainb/noisereduce) (Tim Sainburg).
- Paulstretch: the extreme time-stretch algorithm by Nasca Octavian Paul.
- Reverb: the Freeverb design by Jezar at Dreampoint (public domain).
- Granular synthesis after Barry Truax and Curtis Roads.

## Fonts

Embedded via egui; their licence texts are in [`licenses/`](licenses):

- Ubuntu Light — © Canonical Ltd., Ubuntu Font Licence 1.0
- Hack — © Source Foundry Authors, MIT + Bitstream Vera licence
- Noto Emoji — © Google LLC, SIL Open Font Licence 1.1
- emoji-icon-font — MIT (glyphs from Font Awesome, SIL OFL 1.1)
