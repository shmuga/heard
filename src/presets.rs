//! Node-sequence presets: built-in cleanup chains, built-in experimental patches and user
//! presets stored in `library/_presets/<name>.json`.

use crate::graph::{self, Graph, NodeData, chain_graph};
use crate::ops::{Op, Trim};
use egui::pos2;
use egui_snarl::{InPinId, OutPinId};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PresetKind {
    /// Cleanup chains.
    #[serde(alias = "Look")]
    Clean,
    #[serde(alias = "Glitch")]
    Experimental,
    User,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub kind: PresetKind,
    pub graph: Graph,
}

fn trim() -> Op {
    Op::Trim(Trim::default())
}
fn hpf(freq: f32) -> Op {
    Op::HighPass { freq, slope: 1 }
}
fn nr(amount: f32) -> Op {
    Op::NoiseReduction {
        amount,
        sensitivity: 1.5,
        smooth: 0.5,
        profile: [0.0, 0.0],
    }
}
fn fade(i: f32, o: f32) -> Op {
    Op::Fade {
        fade_in: i,
        fade_out: o,
        curve: 1,
    }
}
fn peak(db: f32) -> Op {
    Op::Normalize {
        mode: 0,
        target: db,
    }
}
fn lufs(db: f32) -> Op {
    Op::Normalize {
        mode: 1,
        target: db,
    }
}

pub fn builtin() -> Vec<Preset> {
    use PresetKind::*;
    let p = |name: &str, kind, graph| Preset {
        name: name.into(),
        kind,
        graph,
    };
    vec![
        p("Neutral", Clean, graph::default_graph()),
        p(
            "Field Clean",
            Clean,
            chain_graph(vec![
                trim(),
                hpf(80.0),
                nr(12.0),
                Op::DeClick {
                    sensitivity: 3.0,
                    width: 0.6,
                },
                fade(5.0, 40.0),
                peak(-1.0),
            ]),
        ),
        p(
            "Voice",
            Clean,
            chain_graph(vec![
                trim(),
                hpf(100.0),
                Op::Hum {
                    base: 0,
                    harmonics: 6,
                    q: 30.0,
                },
                nr(10.0),
                Op::Eq {
                    low_db: -2.0,
                    low_hz: 200.0,
                    mid_db: 2.0,
                    mid_hz: 3000.0,
                    mid_q: 0.9,
                    high_db: 0.0,
                    high_hz: 8000.0,
                },
                Op::Compressor {
                    threshold: -22.0,
                    ratio: 3.0,
                    attack: 8.0,
                    release: 120.0,
                    makeup: 2.0,
                },
                fade(5.0, 60.0),
                lufs(-18.0),
            ]),
        ),
        p(
            "Percussion Hit",
            Clean,
            chain_graph(vec![
                trim(),
                Op::AutoTrim {
                    threshold: -45.0,
                    pad: 4.0,
                },
                hpf(40.0),
                Op::Gate {
                    threshold: -42.0,
                    attack: 0.5,
                    hold: 30.0,
                    release: 80.0,
                    floor: -80.0,
                },
                fade(0.0, 25.0),
                peak(-0.3),
            ]),
        ),
        p(
            "Drone Loop",
            Clean,
            chain_graph(vec![
                trim(),
                hpf(30.0),
                nr(8.0),
                Op::Loop { crossfade: 600.0 },
                lufs(-20.0),
            ]),
        ),
        p(
            "Hum Kill",
            Clean,
            chain_graph(vec![
                trim(),
                hpf(40.0),
                Op::Hum {
                    base: 0,
                    harmonics: 10,
                    q: 40.0,
                },
                peak(-1.0),
            ]),
        ),
        p(
            "Hiss Kill",
            Clean,
            chain_graph(vec![
                trim(),
                nr(18.0),
                Op::LowPass {
                    freq: 11000.0,
                    slope: 0,
                },
                peak(-1.0),
            ]),
        ),
        // ---- experimental
        p(
            "Paulstretch Cloud",
            Experimental,
            chain_graph(vec![
                trim(),
                hpf(40.0),
                Op::Stretch {
                    factor: 12.0,
                    window: 0.35,
                    seed: 1,
                },
                Op::Reverb {
                    size: 0.8,
                    damping: 0.6,
                    mix: 0.3,
                    predelay: 30.0,
                    tail: 4.0,
                },
                fade(400.0, 1500.0),
                lufs(-20.0),
            ]),
        ),
        p(
            "Frozen Moment",
            Experimental,
            chain_graph(vec![
                trim(),
                Op::Freeze {
                    position: 0.3,
                    length: 10.0,
                    smear: 0.25,
                    seed: 4,
                },
                Op::Reverb {
                    size: 0.9,
                    damping: 0.5,
                    mix: 0.4,
                    predelay: 40.0,
                    tail: 5.0,
                },
                fade(800.0, 2000.0),
                lufs(-20.0),
            ]),
        ),
        p(
            "Granular Rain",
            Experimental,
            chain_graph(vec![
                trim(),
                Op::Granular {
                    grain: 45.0,
                    density: 90.0,
                    spread: 0.5,
                    pitch: 5.0,
                    reverse: 0.3,
                    length: 3.0,
                    seed: 11,
                },
                Op::Echo {
                    time: 240.0,
                    feedback: 0.35,
                    mix: 0.25,
                    ping_pong: true,
                },
                peak(-1.0),
            ]),
        ),
        p(
            "Reverse Swell",
            Experimental,
            chain_graph(vec![
                trim(),
                Op::Reverse,
                Op::Reverb {
                    size: 0.85,
                    damping: 0.4,
                    mix: 0.55,
                    predelay: 10.0,
                    tail: 3.0,
                },
                Op::Reverse,
                peak(-1.0),
            ]),
        ),
        p(
            "Tape Memory",
            Experimental,
            chain_graph(vec![
                trim(),
                Op::Tape {
                    wow: 0.6,
                    flutter: 0.4,
                    saturation: 0.5,
                    hiss: 0.25,
                    tone: 0.6,
                    seed: 3,
                },
                Op::Varispeed {
                    semitones: -2.0,
                    cents: 0.0,
                },
                peak(-1.0),
            ]),
        ),
        p(
            "Metal Resonator",
            Experimental,
            chain_graph(vec![
                trim(),
                Op::Resonator {
                    freq: 146.8,
                    feedback: 0.93,
                    damping: 0.25,
                    mix: 0.7,
                },
                Op::RingMod {
                    freq: 311.0,
                    mix: 0.25,
                },
                peak(-1.0),
            ]),
        ),
        p(
            "Lo-Fi Sampler",
            Experimental,
            chain_graph(vec![
                trim(),
                Op::BitCrush {
                    bits: 10,
                    downsample: 3,
                },
                Op::LowPass {
                    freq: 7000.0,
                    slope: 1,
                },
                Op::Drive {
                    drive: 6.0,
                    tone: 0.5,
                    mix: 0.6,
                },
                peak(-1.0),
            ]),
        ),
        p(
            "Octave Down",
            Experimental,
            chain_graph(vec![
                trim(),
                Op::Varispeed {
                    semitones: -12.0,
                    cents: 0.0,
                },
                hpf(25.0),
                peak(-1.0),
            ]),
        ),
        p(
            "Stutter Edit",
            Experimental,
            chain_graph(vec![
                trim(),
                Op::Stutter {
                    slice: 70.0,
                    chance: 0.35,
                    repeats: 3,
                    seed: 8,
                },
                peak(-1.0),
            ]),
        ),
        p("Shadow Double", Experimental, shadow_double()),
        p("Crushed Highs", Experimental, crushed_highs()),
        p("Gated Reverb", Experimental, gated_reverb()),
    ]
}

/// A branching patch: the trimmed sound layered with an octave-down, reversed, reverberant
/// copy of itself.
fn shadow_double() -> Graph {
    let mut g = Graph::new();
    let src = g.insert_node(pos2(0.0, 0.0), NodeData::new(Op::Source));
    let tr = g.insert_node(pos2(290.0, 0.0), NodeData::new(trim()));
    let down = g.insert_node(
        pos2(580.0, 200.0),
        NodeData::new(Op::Varispeed {
            semitones: -12.0,
            cents: 0.0,
        }),
    );
    let rev = g.insert_node(pos2(870.0, 200.0), NodeData::new(Op::Reverse));
    let verb = g.insert_node(
        pos2(1160.0, 200.0),
        NodeData::new(Op::Reverb {
            size: 0.8,
            damping: 0.5,
            mix: 0.6,
            predelay: 20.0,
            tail: 3.0,
        }),
    );
    let mix = g.insert_node(
        pos2(1450.0, 0.0),
        NodeData::new(Op::Mix {
            mode: 0,
            level: 0.5,
            offset: 0.0,
        }),
    );
    let norm = g.insert_node(pos2(1740.0, 0.0), NodeData::new(peak(-1.0)));
    let out = g.insert_node(pos2(2030.0, 0.0), NodeData::new(Op::Output));
    let w = |g: &mut Graph, a, b, i| {
        g.connect(
            OutPinId { node: a, output: 0 },
            InPinId { node: b, input: i },
        );
    };
    w(&mut g, src, tr, 0);
    w(&mut g, tr, mix, 0);
    w(&mut g, tr, down, 0);
    w(&mut g, down, rev, 0);
    w(&mut g, rev, verb, 0);
    w(&mut g, verb, mix, 1);
    w(&mut g, mix, norm, 0);
    w(&mut g, norm, out, 0);
    g
}

fn wire(g: &mut Graph, a: egui_snarl::NodeId, out: usize, b: egui_snarl::NodeId, i: usize) {
    g.connect(
        OutPinId {
            node: a,
            output: out,
        },
        InPinId { node: b, input: i },
    );
}

/// Split into bands, bit-crush only the highs, add the three back together.
fn crushed_highs() -> Graph {
    let mut g = Graph::new();
    let src = g.insert_node(pos2(0.0, 0.0), NodeData::new(Op::Source));
    let tr = g.insert_node(pos2(290.0, 0.0), NodeData::new(trim()));
    let split = g.insert_node(
        pos2(580.0, 0.0),
        NodeData::new(Op::Split {
            low_hz: 300.0,
            high_hz: 3000.0,
        }),
    );
    let crush = g.insert_node(
        pos2(870.0, 260.0),
        NodeData::new(Op::BitCrush {
            bits: 5,
            downsample: 6,
        }),
    );
    let layer = || Op::Mix {
        mode: 0,
        level: 1.0,
        offset: 0.0,
    };
    let m1 = g.insert_node(pos2(1160.0, 0.0), NodeData::new(layer()));
    let m2 = g.insert_node(pos2(1450.0, 0.0), NodeData::new(layer()));
    let norm = g.insert_node(pos2(1740.0, 0.0), NodeData::new(peak(-1.0)));
    let out = g.insert_node(pos2(2030.0, 0.0), NodeData::new(Op::Output));
    wire(&mut g, src, 0, tr, 0);
    wire(&mut g, tr, 0, split, 0);
    wire(&mut g, split, 0, m1, 0);
    wire(&mut g, split, 1, m1, 1);
    wire(&mut g, split, 2, crush, 0);
    wire(&mut g, m1, 0, m2, 0);
    wire(&mut g, crush, 0, m2, 1);
    wire(&mut g, m2, 0, norm, 0);
    wire(&mut g, norm, 0, out, 0);
    g
}

/// The 80s trick: a big reverb that's cut off whenever the dry sound stops.
fn gated_reverb() -> Graph {
    let mut g = Graph::new();
    let src = g.insert_node(pos2(0.0, 0.0), NodeData::new(Op::Source));
    let tr = g.insert_node(pos2(290.0, 0.0), NodeData::new(trim()));
    let verb = g.insert_node(
        pos2(580.0, 0.0),
        NodeData::new(Op::Reverb {
            size: 0.95,
            damping: 0.3,
            mix: 0.7,
            predelay: 5.0,
            tail: 2.0,
        }),
    );
    let gate = g.insert_node(
        pos2(870.0, 0.0),
        NodeData::new(Op::Sidechain {
            mode: 0,
            threshold: -28.0,
            depth: 1.0,
            release: 60.0,
        }),
    );
    let norm = g.insert_node(pos2(1160.0, 0.0), NodeData::new(peak(-1.0)));
    let out = g.insert_node(pos2(1450.0, 0.0), NodeData::new(Op::Output));
    wire(&mut g, src, 0, tr, 0);
    wire(&mut g, tr, 0, verb, 0);
    wire(&mut g, verb, 0, gate, 0);
    wire(&mut g, tr, 0, gate, 1);
    wire(&mut g, gate, 0, norm, 0);
    wire(&mut g, norm, 0, out, 0);
    g
}

/// Apply a preset to the current graph. The sound's own cut is kept (its Trim node), and so
/// is a learned noise profile (it belongs to this recording, not to the preset).
pub fn apply(current: &Graph, preset: &Graph) -> Graph {
    let mut g = preset.clone();
    if let Some(cur) = graph::trim_node(current).map(|id| current[id].clone()) {
        match graph::trim_node(&g) {
            Some(pt) => g[pt] = cur,
            None => {
                if let Some(src) = graph::source_node(&g) {
                    let id = graph::insert_after(&mut g, src, Op::Output);
                    g[id] = cur;
                }
            }
        }
    }
    let is_nr = |o: &Op| matches!(o, Op::NoiseReduction { .. });
    let learned = graph::find(current, is_nr).and_then(|id| match &current[id].op {
        Op::NoiseReduction { profile, .. } if profile[1] > profile[0] => Some(*profile),
        _ => None,
    });
    if let Some(lp) = learned
        && let Some(id) = graph::find(&g, is_nr)
        && let Op::NoiseReduction { profile, .. } = &mut g[id].op
    {
        *profile = lp;
    }
    g
}

pub fn load_user(dir: &Path) -> Vec<Preset> {
    let mut v: Vec<Preset> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| {
            serde_json::from_str::<Preset>(&std::fs::read_to_string(e.path()).ok()?).ok()
        })
        .map(|mut p| {
            p.kind = PresetKind::User;
            p
        })
        .collect();
    v.sort_by_key(|a| a.name.to_lowercase());
    v
}

pub fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim()
        .to_string()
}

pub fn save_user(dir: &Path, name: &str, graph: &Graph) -> anyhow::Result<Preset> {
    std::fs::create_dir_all(dir)?;
    let p = Preset {
        name: name.trim().to_string(),
        kind: PresetKind::User,
        graph: graph.clone(),
    };
    let file = dir.join(format!("{}.json", sanitize_name(name)));
    std::fs::write(file, serde_json::to_string_pretty(&p)?)?;
    Ok(p)
}

pub fn delete_user(dir: &Path, name: &str) -> anyhow::Result<()> {
    std::fs::remove_file(dir.join(format!("{}.json", sanitize_name(name))))?;
    Ok(())
}
