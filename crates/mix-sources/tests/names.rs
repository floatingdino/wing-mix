//! Name-heuristics table: common console scribble-strip abbreviations.

use mix_sources::{Identifier, SourceType, SourceType::*, StripInput};

const TABLE: &[(&str, SourceType)] = &[
    // Kick
    ("KICK", Kick),
    ("Kick In", Kick),
    ("KICK OUT", Kick),
    ("KK", Kick),
    ("KIK", Kick),
    ("BD", Kick),
    ("Bass Drum", Kick),
    ("03-Kick", Kick),
    ("KCK SUB", Kick),
    // Snare
    ("SN", SnareTop),
    ("SNARE", SnareTop),
    ("SN TOP", SnareTop),
    ("SNR", SnareTop),
    ("SD", SnareTop),
    ("SN BOT", SnareBottom),
    ("SNARE BTM", SnareBottom),
    ("Snare Bottom", SnareBottom),
    ("SN B", SnareBottom),
    // Hats / toms / overheads / room
    ("HH", HiHat),
    ("HATS", HiHat),
    ("Hi-Hat", HiHat),
    ("HiHat", HiHat),
    ("TOM", TomRack),
    ("TOM1", TomRack),
    ("Tom 2", TomRack),
    ("RACK", TomRack),
    ("T1", TomRack),
    ("FT", TomFloor),
    ("FLOOR", TomFloor),
    ("Floor Tom", TomFloor),
    ("TOM 4", TomFloor),
    ("Tom 3", TomFloor),
    ("OH", Overheads),
    ("OH L", Overheads),
    ("OHR", Overheads),
    ("OVH L", Overheads),
    ("Overheads", Overheads),
    ("RIDE", Overheads),
    ("CYM", Overheads),
    ("ROOM", Room),
    ("AMB L", Room),
    ("RM R", Room),
    ("Audience L", Room),
    // Percussion / sampler
    ("PERC", Percussion),
    ("SHAKER", Percussion),
    ("TAMB", Percussion),
    ("CAJON", Percussion),
    ("CONGAS", Percussion),
    ("SPD", Sampler),
    ("SPD-SX L", Sampler),
    // Bass
    ("BASS", BassDi),
    ("BS", BassDi),
    ("BS DI", BassDi),
    ("BASS DI", BassDi),
    ("BASS DRY", BassDi),
    ("Bass Amp", BassAmp),
    ("BS MIC", BassAmp),
    ("BASS CAB", BassAmp),
    ("SYN BASS", BassDi),
    // Guitars
    ("GTR", ElectricGuitar),
    ("GTR 1", ElectricGuitar),
    ("EG", ElectricGuitar),
    ("E GTR L", ElectricGuitar),
    ("ELEC", ElectricGuitar),
    ("LEAD GTR", ElectricGuitar),
    ("Guitar", ElectricGuitar),
    ("GTRL", ElectricGuitar),
    ("AG", AcousticGuitar),
    ("AC", AcousticGuitar),
    ("AC GTR", AcousticGuitar),
    ("ACOUSTIC", AcousticGuitar),
    ("AGTR", AcousticGuitar),
    ("ACG", AcousticGuitar),
    ("ACG 2", AcousticGuitar),
    ("AG L", AcousticGuitar),
    ("22-AC", AcousticGuitar),
    // Keys
    ("KEYS", KeysSynth),
    ("KEYS L", KeysSynth),
    ("KBD", KeysSynth),
    ("SYN", KeysSynth),
    ("SYNTH R", KeysSynth),
    ("PAD", KeysSynth),
    ("ORGAN", KeysSynth),
    ("B3", KeysSynth),
    ("RHODES", KeysSynth),
    ("EP", KeysSynth),
    ("Nord L", KeysSynth),
    ("PNO", Piano),
    ("PIANO L", Piano),
    ("Grand", Piano),
    // Vocals
    ("VOX", LeadVocal),
    ("VOX 1", LeadVocal),
    ("LV", LeadVocal),
    ("LEAD VOX", LeadVocal),
    ("LD VOX", LeadVocal),
    ("Vocal", LeadVocal),
    ("VOC", LeadVocal),
    ("38-VOX 1", LeadVocal),
    ("BV", BackingVocal),
    ("BV 1", BackingVocal),
    ("BVS", BackingVocal),
    ("BVOX 2", BackingVocal),
    ("BGV", BackingVocal),
    ("BKG VOX", BackingVocal),
    ("Choir L", BackingVocal),
    ("HARM", BackingVocal),
    // Speech / talkback
    ("MC", Speech),
    ("Pastor", Speech),
    ("Lectern", Speech),
    ("HOST", Speech),
    ("LAV 1", Speech),
    ("TB", Talkback),
    ("Talkback", Talkback),
    ("MD", Talkback),
    ("35-MD", Talkback),
    ("MD MIC", Talkback),
    // Playback / click
    ("TX", Playback),
    ("TX L", Playback),
    ("TRACKS", Playback),
    ("TRAX R", Playback),
    ("PB L", Playback),
    ("Playback", Playback),
    ("USB L", Playback),
    ("Laptop", Playback),
    ("BGM", Playback),
    ("Walk In", Playback),
    ("Stems", Playback),
    ("CLICK", Click),
    ("CLK", Click),
    ("Metronome", Click),
    // Brass / strings
    ("TPT", Brass),
    ("SAX", Brass),
    ("Trombone", Brass),
    ("HORNS", Brass),
    ("VLN", Strings),
    ("Cello", Strings),
    ("STRINGS", Strings),
];

#[test]
fn name_table() {
    let id = Identifier::default();
    let mut bad = Vec::new();
    for &(name, want) in TABLE {
        let c = id.classify(&StripInput::named(name));
        if c.source() != Some(want) {
            bad.push(format!(
                "{name:?}: want {want}, got {:?} ({:.2})",
                c.source(),
                c.confidence()
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{} of {} wrong:\n{}",
        bad.len(),
        TABLE.len(),
        bad.join("\n")
    );
}

#[test]
fn unnamed_or_unknown_names_give_no_source() {
    let id = Identifier::default();
    for name in ["", "28", "CH 28", "Input 12", "Dave", "Sarah"] {
        assert_eq!(
            id.classify(&StripInput::named(name)).source(),
            None,
            "{name}"
        );
    }
}

#[test]
fn knowledge_base_fallback_catches_compact_names() {
    // Not a rule token, but the knowledge base's substring hints know it.
    let c = Identifier::default().classify(&StripInput::named("KICKSUB"));
    assert_eq!(c.best().map(|c| c.source), Some(Kick));
}
