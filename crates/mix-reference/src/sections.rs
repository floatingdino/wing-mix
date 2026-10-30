//! Section segmentation and labelling for reference songs (analysis.md §3.7).
//!
//! Boundaries come from mix-analysis' Foote novelty. Each segment is summarised by the median of its 40 ERB
//! loudness-relative band levels and its short-term loudness; segments are grouped by average-linkage
//! agglomerative clustering on a dB-scale distance (RMS band difference combined with the loudness difference in
//! LU; threshold [`CLUSTER_THRESHOLD_DB`], deliberately interpretable instead of cosine on z-scores, which blows up
//! noise when segments are similar). Labels are hints: the loudest repeated cluster is "chorus", other repeated
//! clusters "verse", a unique quiet first/last segment "intro"/"outro", anything else "bridge".

use mix_analysis::{OfflineAnalysis, ProfileBuilder};
use mix_core::bands::N_ERB;
use mix_core::SectionRef;

/// Segments closer than this (RMS dB over the ERB bands, plus loudness LU) are the same cluster.
pub const CLUSTER_THRESHOLD_DB: f32 = 3.0;
/// A trailing segment shorter than this is merged into its predecessor.
pub const MIN_SEGMENT_S: f64 = 4.0;

#[derive(Clone)]
struct SegSummary {
    erb: Vec<f32>,
    loud: f32,
}

/// Splits the song at the novelty boundaries and labels the segments. Always returns at least one section.
pub fn detect_sections(a: &OfflineAnalysis) -> Vec<SectionRef> {
    let mut bounds: Vec<f64> = vec![0.0];
    if let Some(n) = &a.novelty {
        bounds.extend(
            n.boundaries_s
                .iter()
                .copied()
                .filter(|&b| b > 0.0 && b < a.duration_s),
        );
    }
    bounds.push(a.duration_s);
    // Merge a short tail into the previous segment.
    while bounds.len() > 2 && bounds[bounds.len() - 1] - bounds[bounds.len() - 2] < MIN_SEGMENT_S {
        let end = bounds.pop().unwrap();
        *bounds.last_mut().unwrap() = end;
    }
    // Millisecond resolution keeps the JSON text of the times exactly round-trippable.
    let ms = |x: f64| (x * 1000.0).round() / 1000.0;
    let spans: Vec<(f64, f64)> = bounds.windows(2).map(|w| (ms(w[0]), ms(w[1]))).collect();
    if spans.len() == 1 {
        return vec![SectionRef {
            index: 0,
            label: "song".into(),
            start_s: spans[0].0,
            end_s: spans[0].1,
        }];
    }

    let pb = ProfileBuilder::new(a);
    let sums: Vec<SegSummary> = spans.iter().map(|&(s, e)| summarise(&pb, s, e)).collect();
    let cluster = cluster(&sums);
    let labels = label(&sums, &cluster);
    spans
        .iter()
        .zip(labels)
        .enumerate()
        .map(|(i, (&(s, e), label))| SectionRef {
            index: i as u32,
            label,
            start_s: s,
            end_s: e,
        })
        .collect()
}

fn median(mut v: Vec<f32>) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    v[v.len() / 2]
}

fn summarise(pb: &ProfileBuilder<'_>, s: f64, e: f64) -> SegSummary {
    let w = pb.windows(s, e);
    SegSummary {
        erb: (0..N_ERB)
            .map(|b| median(w.iter().map(|f| f.spectrum.erb_mid_rel_db[b]).collect()))
            .collect(),
        loud: median(w.iter().map(|f| f.loudness.short_term_lufs).collect()),
    }
}

fn distance(a: &SegSummary, b: &SegSummary) -> f32 {
    let band = a
        .erb
        .iter()
        .zip(&b.erb)
        .map(|(x, y)| (x - y) * (x - y))
        .sum::<f32>()
        / N_ERB as f32;
    (band + (a.loud - b.loud).powi(2)).sqrt()
}

/// Average-linkage agglomerative clustering; returns a cluster id per segment (ids in order of first appearance).
fn cluster(sums: &[SegSummary]) -> Vec<usize> {
    let mut groups: Vec<Vec<usize>> = (0..sums.len()).map(|i| vec![i]).collect();
    loop {
        let mut best: Option<(usize, usize, f32)> = None;
        for i in 0..groups.len() {
            for j in i + 1..groups.len() {
                let mut d = 0.0;
                for &x in &groups[i] {
                    for &y in &groups[j] {
                        d += distance(&sums[x], &sums[y]);
                    }
                }
                d /= (groups[i].len() * groups[j].len()) as f32;
                if best.is_none_or(|(_, _, bd)| d < bd) {
                    best = Some((i, j, d));
                }
            }
        }
        match best {
            Some((i, j, d)) if d < CLUSTER_THRESHOLD_DB => {
                let g = groups.remove(j);
                groups[i].extend(g);
            }
            _ => break,
        }
    }
    groups.sort_by_key(|g| *g.iter().min().unwrap());
    let mut ids = vec![0; sums.len()];
    for (c, g) in groups.iter().enumerate() {
        for &i in g {
            ids[i] = c;
        }
    }
    ids
}

fn label(sums: &[SegSummary], cluster: &[usize]) -> Vec<String> {
    let n = sums.len();
    let n_clusters = cluster.iter().max().map_or(0, |m| m + 1);
    let members = |c: usize| cluster.iter().filter(|&&x| x == c).count();
    let mean_loud = |c: usize| {
        let v: Vec<f32> = (0..n).filter(|&i| cluster[i] == c).map(|i| sums[i].loud).collect();
        v.iter().sum::<f32>() / v.len() as f32
    };
    let chorus = (0..n_clusters)
        .filter(|&c| members(c) >= 2)
        .max_by(|&a, &b| mean_loud(a).total_cmp(&mean_loud(b)));
    let mid = median(sums.iter().map(|s| s.loud).collect());
    let quiet_unique = |i: usize| members(cluster[i]) == 1 && sums[i].loud < mid - 1.5;

    #[derive(Clone, Copy, PartialEq)]
    enum Kind {
        Intro,
        Outro,
        Chorus,
        Verse,
        Bridge,
    }
    let kinds: Vec<Kind> = (0..n)
        .map(|i| {
            if Some(cluster[i]) == chorus {
                Kind::Chorus
            } else if i == 0 && quiet_unique(i) {
                Kind::Intro
            } else if i == n - 1 && quiet_unique(i) {
                Kind::Outro
            } else if members(cluster[i]) >= 2 {
                Kind::Verse
            } else {
                Kind::Bridge
            }
        })
        .collect();
    let mut counts = [0usize; 5];
    kinds
        .iter()
        .map(|&k| match k {
            Kind::Intro => "intro".to_string(),
            Kind::Outro => "outro".to_string(),
            Kind::Chorus | Kind::Verse | Kind::Bridge => {
                let (slot, name) = match k {
                    Kind::Chorus => (2, "chorus"),
                    Kind::Verse => (3, "verse"),
                    _ => (4, "bridge"),
                };
                counts[slot] += 1;
                format!("{name} {}", counts[slot])
            }
        })
        .collect()
}
