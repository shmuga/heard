# Changelog

## 0.2.0

### New
- **Export library…** (library toolbar, `:exportlib`): every filed sound goes into one flat
  folder as `tags_collection_name.wav`, using its saved edit or, if untouched, the original
  (as WAV). The Inbox and the special **Raw** collection are skipped. An empty folder is used
  as it is; otherwise a dated sub-folder is made, so nothing is overwritten.
- **Trim to selection on the result** (`C`, `:trim sel`, Trim card button): the selection maps
  back to the sound's trim. After a time-changing node (Paulstretch, Varispeed, Reverse…) a
  second Trim is added just before Output to crop what you hear.
- **Snap to zero crossings** (toolbar toggle, `:set snap` / `:set nosnap`, Settings, on by
  default): selection and trim edges land on zero crossings, so cuts don't click.

### Fixed
- Trim handles couldn't be dragged: a drag that started on a handle became a selection.
- After a length-changing edit (e.g. an extreme Paulstretch), playback kept to the old
  length: the old selection or loop range was kept.
- Short sounds: the waveform drew 32-sample blocks as wide steps. It now draws the samples.
  The spectrogram's low end was blocky and striped; it now uses a longer window below
  ~700 Hz and a finer frequency grid.

## 0.1.0

First version: library of found sounds (Inbox + collections as plain folders), non-destructive
recipes (sliders or node patch), clean-up and experimental nodes, Mix / Sidechain / Split
routing, presets, slicing with markers, vim-style keys.
