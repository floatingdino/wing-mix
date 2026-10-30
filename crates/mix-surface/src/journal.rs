//! Change journal with undo/redo and session revert.
//!
//! Every applied batch is recorded with the value each param had before (`from`) and the value the surface stored
//! (`to`). Undo restores `from` in reverse order, but only for params still at `to`: if the engineer (or another
//! client) moved a param since, that param is left alone and reported as a [`Conflict`] (the engineer's state is
//! sacred). Redo is symmetric. The append-only [`Journal::entries`] log also records confirmations and external
//! changes for the session audit trail (M050 persists it).

use crate::surface::{Surface, SurfaceError};
use mix_core::ids::ParamId;
use mix_core::value::Value;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// One param move.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Change {
    pub param: ParamId,
    pub from: Value,
    pub to: Value,
}

/// Changes applied (and undone) together.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Batch {
    pub id: u64,
    pub label: String,
    pub changes: Vec<Change>,
}

/// A param that was not restored because it no longer had the expected value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Conflict {
    pub param: ParamId,
    pub expected: Value,
    pub found: Value,
}

/// Outcome of an undo or redo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepReport {
    pub batch: u64,
    pub label: String,
    pub restored: Vec<ParamId>,
    pub conflicts: Vec<Conflict>,
}

/// Append-only audit log entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Entry {
    Applied {
        batch: Batch,
    },
    Undone {
        report: StepReport,
    },
    Redone {
        report: StepReport,
    },
    /// A remit confirmation (`<rule>`, `coupling:<rule>`, `revive:<rule>`) or role confirmation.
    Confirmed {
        key: String,
    },
    Revoked {
        key: String,
    },
    /// Someone else changed a param during the session.
    External {
        param: ParamId,
        value: Value,
    },
    Note {
        text: String,
    },
}

/// Timestamped log entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Logged {
    /// Milliseconds since the Unix epoch.
    pub at_ms: u64,
    pub entry: Entry,
}

/// The session journal.
#[derive(Debug, Clone, Default)]
pub struct Journal {
    log: Vec<Logged>,
    undo: Vec<Batch>,
    redo: Vec<Batch>,
    next_id: u64,
    baseline: BTreeMap<ParamId, Value>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn tolerance(surface: &dyn Surface, p: &ParamId) -> f64 {
    surface.param_spec(p).map(|s| s.tolerance()).unwrap_or(1e-6)
}

impl Journal {
    pub fn new() -> Self {
        Self::default()
    }

    fn push(&mut self, entry: Entry) {
        self.log.push(Logged {
            at_ms: now_ms(),
            entry,
        });
    }

    /// Record a batch that was already applied. Clears the redo stack. Returns the batch id.
    pub fn record(&mut self, label: impl Into<String>, changes: Vec<Change>) -> u64 {
        self.next_id += 1;
        for c in &changes {
            self.baseline
                .entry(c.param.clone())
                .or_insert_with(|| c.from.clone());
        }
        let batch = Batch {
            id: self.next_id,
            label: label.into(),
            changes,
        };
        self.redo.clear();
        self.push(Entry::Applied {
            batch: batch.clone(),
        });
        self.undo.push(batch);
        self.next_id
    }

    /// Read, write and record a batch. If a write fails, the params already written in this batch are restored and
    /// the error is returned (nothing is recorded).
    pub fn apply(
        &mut self,
        surface: &dyn Surface,
        label: impl Into<String>,
        moves: &[(ParamId, Value)],
    ) -> Result<u64, SurfaceError> {
        let mut done: Vec<Change> = Vec::new();
        for (p, v) in moves {
            let res = surface
                .read(p)
                .and_then(|from| surface.write(p, v.clone()).map(|to| (from, to)));
            match res {
                Ok((from, to)) => done.push(Change {
                    param: p.clone(),
                    from,
                    to,
                }),
                Err(e) => {
                    for c in done.iter().rev() {
                        let _ = surface.write(&c.param, c.from.clone());
                    }
                    return Err(e);
                }
            }
        }
        Ok(self.record(label, done))
    }

    /// Undo the last batch. `Ok(None)` when there is nothing to undo.
    pub fn undo(&mut self, surface: &dyn Surface) -> Result<Option<StepReport>, SurfaceError> {
        let Some(batch) = self.undo.pop() else {
            return Ok(None);
        };
        let (report, kept) = Self::step(surface, &batch, true)?;
        if !kept.is_empty() {
            self.redo.push(Batch {
                changes: kept,
                ..batch
            });
        }
        self.push(Entry::Undone {
            report: report.clone(),
        });
        Ok(Some(report))
    }

    /// Redo the last undone batch.
    pub fn redo(&mut self, surface: &dyn Surface) -> Result<Option<StepReport>, SurfaceError> {
        let Some(batch) = self.redo.pop() else {
            return Ok(None);
        };
        let (report, kept) = Self::step(surface, &batch, false)?;
        if !kept.is_empty() {
            self.undo.push(Batch {
                changes: kept,
                ..batch
            });
        }
        self.push(Entry::Redone {
            report: report.clone(),
        });
        Ok(Some(report))
    }

    /// PANIC revert: undo every batch of the session (newest first).
    pub fn revert_all(&mut self, surface: &dyn Surface) -> Result<Vec<StepReport>, SurfaceError> {
        let mut out = Vec::new();
        while let Some(r) = self.undo(surface)? {
            out.push(r);
        }
        Ok(out)
    }

    /// Apply one batch backwards (`undo`) or forwards. Returns the report and the changes actually applied.
    fn step(
        surface: &dyn Surface,
        batch: &Batch,
        undo: bool,
    ) -> Result<(StepReport, Vec<Change>), SurfaceError> {
        let mut restored = Vec::new();
        let mut conflicts = Vec::new();
        let mut kept = Vec::new();
        let order: Box<dyn Iterator<Item = &Change>> = if undo {
            Box::new(batch.changes.iter().rev())
        } else {
            Box::new(batch.changes.iter())
        };
        for c in order {
            let (expect, target) = if undo {
                (&c.to, &c.from)
            } else {
                (&c.from, &c.to)
            };
            let found = surface.read(&c.param)?;
            if found.approx_eq(expect, tolerance(surface, &c.param)) {
                surface.write(&c.param, target.clone())?;
                restored.push(c.param.clone());
                kept.push(c.clone());
            } else {
                conflicts.push(Conflict {
                    param: c.param.clone(),
                    expected: expect.clone(),
                    found,
                });
            }
        }
        if undo {
            kept.reverse();
        }
        let report = StepReport {
            batch: batch.id,
            label: batch.label.clone(),
            restored,
            conflicts,
        };
        Ok((report, kept))
    }

    pub fn note_confirmed(&mut self, key: impl Into<String>) {
        self.push(Entry::Confirmed { key: key.into() });
    }

    pub fn note_revoked(&mut self, key: impl Into<String>) {
        self.push(Entry::Revoked { key: key.into() });
    }

    pub fn note_external(&mut self, param: ParamId, value: Value) {
        self.push(Entry::External { param, value });
    }

    pub fn note(&mut self, text: impl Into<String>) {
        self.push(Entry::Note { text: text.into() });
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// The audit log.
    pub fn entries(&self) -> &[Logged] {
        &self.log
    }

    /// Value a param had before the session first touched it (envelope baseline).
    pub fn baseline(&self, param: &ParamId) -> Option<&Value> {
        self.baseline.get(param)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::FakeSurface;

    fn p(s: &str) -> ParamId {
        ParamId::new(s)
    }

    #[test]
    fn undo_redo_round_trip() {
        let s = FakeSurface::demo();
        let mut j = Journal::new();
        let f0 = s.read(&p("input.1/fader")).unwrap();
        let g0 = s.read(&p("input.1/eq/band.1.gain")).unwrap();
        j.apply(&s, "level", &[(p("input.1/fader"), Value::Float(-6.0))])
            .unwrap();
        j.apply(
            &s,
            "eq",
            &[
                (p("input.1/eq/band.1.gain"), Value::Float(-3.0)),
                (p("input.1/eq/band.1.freq"), Value::Float(250.0)),
            ],
        )
        .unwrap();
        assert_eq!(
            s.read(&p("input.1/eq/band.1.gain")).unwrap(),
            Value::Float(-3.0)
        );

        let r = j.undo(&s).unwrap().unwrap();
        assert_eq!(r.label, "eq");
        assert_eq!(r.restored.len(), 2);
        assert_eq!(
            r.restored[0],
            p("input.1/eq/band.1.freq"),
            "undo runs in reverse order"
        );
        assert_eq!(s.read(&p("input.1/eq/band.1.gain")).unwrap(), g0);
        assert!(j.can_redo());

        let r = j.redo(&s).unwrap().unwrap();
        assert_eq!(r.restored.len(), 2);
        assert_eq!(
            s.read(&p("input.1/eq/band.1.gain")).unwrap(),
            Value::Float(-3.0)
        );

        let all = j.revert_all(&s).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(s.read(&p("input.1/fader")).unwrap(), f0);
        assert_eq!(s.read(&p("input.1/eq/band.1.gain")).unwrap(), g0);
        assert!(!j.can_undo());
        assert!(j.undo(&s).unwrap().is_none());
        assert_eq!(j.baseline(&p("input.1/fader")), Some(&f0));
    }

    #[test]
    fn new_record_clears_redo() {
        let s = FakeSurface::demo();
        let mut j = Journal::new();
        j.apply(&s, "a", &[(p("input.1/fader"), Value::Float(-6.0))])
            .unwrap();
        j.undo(&s).unwrap();
        assert!(j.can_redo());
        j.apply(&s, "b", &[(p("input.2/fader"), Value::Float(-6.0))])
            .unwrap();
        assert!(!j.can_redo());
    }

    #[test]
    fn undo_skips_params_the_engineer_moved() {
        let s = FakeSurface::demo();
        let mut j = Journal::new();
        j.apply(
            &s,
            "two",
            &[
                (p("input.1/fader"), Value::Float(-6.0)),
                (p("input.2/fader"), Value::Float(-6.0)),
            ],
        )
        .unwrap();
        s.external_set(&p("input.2/fader"), Value::Float(-12.0))
            .unwrap();
        let r = j.undo(&s).unwrap().unwrap();
        assert_eq!(r.restored, vec![p("input.1/fader")]);
        assert_eq!(r.conflicts.len(), 1);
        assert_eq!(r.conflicts[0].param, p("input.2/fader"));
        assert_eq!(
            s.read(&p("input.2/fader")).unwrap(),
            Value::Float(-12.0),
            "engineer's move is kept"
        );
        // Redo only re-applies what was undone.
        let r = j.redo(&s).unwrap().unwrap();
        assert_eq!(r.restored, vec![p("input.1/fader")]);
    }

    #[test]
    fn failed_batch_rolls_back() {
        let s = FakeSurface::demo();
        let f0 = s.read(&p("input.1/fader")).unwrap();
        let mut j = Journal::new();
        let err = j
            .apply(
                &s,
                "bad",
                &[
                    (p("input.1/fader"), Value::Float(-6.0)),
                    (p("input.1/nope"), Value::Float(0.0)),
                ],
            )
            .unwrap_err();
        assert!(matches!(err, SurfaceError::UnknownParam(_)));
        assert_eq!(s.read(&p("input.1/fader")).unwrap(), f0);
        assert!(!j.can_undo());
    }

    #[test]
    fn audit_log() {
        let s = FakeSurface::demo();
        let mut j = Journal::new();
        j.note_confirmed("input-preamp");
        j.apply(&s, "a", &[(p("input.1/fader"), Value::Float(-6.0))])
            .unwrap();
        j.note_external(p("input.1/mute"), Value::Bool(true));
        j.undo(&s).unwrap();
        let kinds: Vec<&str> = j
            .entries()
            .iter()
            .map(|l| match l.entry {
                Entry::Confirmed { .. } => "confirmed",
                Entry::Applied { .. } => "applied",
                Entry::External { .. } => "external",
                Entry::Undone { .. } => "undone",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["confirmed", "applied", "external", "undone"]);
    }
}
