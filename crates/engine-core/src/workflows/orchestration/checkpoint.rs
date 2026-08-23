//! The per-chain checkpoint — `EN.11.H` task 1.
//!
//! A crashed campaign (`kill -9` mid-chain) leaves branches pushed, PRs
//! open, and lane-log lines appended, with nothing on disk recording how
//! far it got. This module is that record: which steps of a campaign's
//! chain already integrated, and which branch each one created, so a
//! `resume <campaign>` (`EN.11.H` task 4) can restart at block N+1
//! without re-creating a branch the crashed run already made.
//!
//! No caller yet — [`super::integrate::integrate_chain`] writes this
//! checkpoint as of task 2. This task only adds the type, its atomic
//! writer, and its "missing means no checkpoint" reader.
//!
//! # On-disk location
//!
//! One checkpoint file per campaign, alongside that campaign's
//! `lane-log.jsonl` — the same `roadmap_dir` [`super::integrate::resolve_roadmap_dir`]
//! already resolves, not a new root. The filename embeds the campaign id
//! so concurrent campaigns against the same roadmap never collide:
//! `checkpoint-<campaign_id>.json`.
//!
//! # Atomicity
//!
//! [`write_checkpoint`] writes the full JSON body to a sibling temp file
//! first, then renames it into place. `rename` on the same filesystem is
//! atomic, so a reader ([`read_checkpoint`]) can never observe a
//! partially-written file — the very crash this checkpoint exists to
//! survive is exactly the kind of interruption that could otherwise tear
//! a direct in-place write. A missing file (no checkpoint has ever been
//! written for this campaign — e.g. a first run) is [`ReadCheckpoint::Absent`],
//! never an error: a fresh campaign having no checkpoint is the expected
//! case, not a failure.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One step's record inside a [`Checkpoint`] — enough to answer both
/// "restart at N+1" (via `index`/`integrated`) and "this branch already
/// exists, do not re-create it" (via `branch`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointStep {
    /// The repo the step's block lives in.
    pub repo: String,
    /// The block id, e.g. `EN.11.H`.
    pub block_id: String,
    /// This step's 1-based position in the chain — matches the chain's
    /// own step ordering, so "resume at N+1" is a plain index comparison.
    pub index: u32,
    /// Whether this step finished integrating (its `SDLC_FLOW` run
    /// completed, its state write verified, and its `lane-log.jsonl` line
    /// was appended). A step recorded here with `integrated: false` is one
    /// whose branch was created but that never finished — resume must
    /// still not fail on its branch, but must not treat it as done either.
    pub integrated: bool,
    /// The branch name this step's `SDLC_FLOW` run created, if any (a
    /// worktree-isolated run creates one; a plain-branch run may not,
    /// depending on the engine). `None` means no branch is known for this
    /// step, not that one wasn't created.
    pub branch: Option<String>,
}

/// The per-chain checkpoint for one campaign: which of its steps already
/// integrated, in chain order, and enough about each to resume at the
/// first one that isn't.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// The campaign this checkpoint belongs to — the same identity
    /// `integrate_chain` already takes as its `campaign_id` parameter
    /// (`EN.11.E`).
    pub campaign_id: Uuid,
    /// Every step recorded so far, in chain order.
    pub steps: Vec<CheckpointStep>,
}

impl Checkpoint {
    /// A fresh, empty checkpoint for `campaign_id` — no steps recorded
    /// yet.
    #[must_use]
    pub fn new(campaign_id: Uuid) -> Self {
        Self {
            campaign_id,
            steps: Vec::new(),
        }
    }
}

/// The result of [`read_checkpoint`]: either a checkpoint was found, or
/// none exists yet. Deliberately not `Option<Checkpoint>` at the call
/// site's surface — spelling out `Absent` reads unambiguously as "no
/// checkpoint", where a bare `None` invites confusing it with an error
/// swallowed to `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadCheckpoint {
    /// A checkpoint was found and parsed.
    Found(Checkpoint),
    /// No checkpoint file exists at this path — the expected state for a
    /// campaign that has never been checkpointed (e.g. a first run, or
    /// one still on its first step).
    Absent,
}

impl ReadCheckpoint {
    /// Convert to the more ergonomic `Option` for callers that don't need
    /// the `Absent` naming.
    #[must_use]
    pub fn into_option(self) -> Option<Checkpoint> {
        match self {
            ReadCheckpoint::Found(checkpoint) => Some(checkpoint),
            ReadCheckpoint::Absent => None,
        }
    }
}

/// Everything that can go wrong writing or reading a checkpoint. A
/// missing file is never one of these variants — see [`ReadCheckpoint::Absent`].
#[derive(Debug)]
pub enum CheckpointError {
    /// The checkpoint (or its temp file) could not be serialized to JSON.
    Serialize {
        campaign_id: Uuid,
        source: serde_json::Error,
    },
    /// The checkpoint could not be parsed as valid JSON — a corrupt or
    /// foreign file at this path, distinct from the file simply not
    /// existing.
    Deserialize {
        path: PathBuf,
        source: serde_json::Error,
    },
    /// The temp file could not be written.
    WriteTempFailed {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The temp file could not be renamed into place.
    RenameFailed {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The checkpoint file exists but could not be read (permissions, a
    /// directory at that path, etc.) — distinct from it not existing.
    ReadFailed {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl std::fmt::Display for CheckpointError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CheckpointError::Serialize {
                campaign_id,
                source,
            } => write!(
                f,
                "failed to serialize checkpoint for campaign {campaign_id}: {source}"
            ),
            CheckpointError::Deserialize { path, source } => write!(
                f,
                "failed to parse checkpoint at {}: {source}",
                path.display()
            ),
            CheckpointError::WriteTempFailed { path, source } => write!(
                f,
                "failed to write checkpoint temp file {}: {source}",
                path.display()
            ),
            CheckpointError::RenameFailed { path, source } => write!(
                f,
                "failed to rename checkpoint temp file into {}: {source}",
                path.display()
            ),
            CheckpointError::ReadFailed { path, source } => write!(
                f,
                "failed to read checkpoint at {}: {source}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for CheckpointError {}

/// The checkpoint path for `campaign_id` under `roadmap_dir` — the same
/// directory [`super::integrate::resolve_roadmap_dir`] resolves and
/// [`super::integrate::append_lane_log_line`] already writes
/// `lane-log.jsonl` into. Never a new root.
#[must_use]
pub fn checkpoint_path(roadmap_dir: &Path, campaign_id: Uuid) -> PathBuf {
    roadmap_dir.join(format!("checkpoint-{campaign_id}.json"))
}

/// Write `checkpoint` to `roadmap_dir`, atomically: the full JSON body is
/// written to a sibling temp file first, then renamed into place. A crash
/// (or a reader) mid-write can only ever observe either the old checkpoint
/// (rename hasn't happened yet) or the new one (rename is atomic) — never
/// a torn, partially-written file.
pub fn write_checkpoint(
    roadmap_dir: &Path,
    checkpoint: &Checkpoint,
) -> Result<(), CheckpointError> {
    let final_path = checkpoint_path(roadmap_dir, checkpoint.campaign_id);
    let tmp_path = final_path.with_extension("json.tmp");

    let body =
        serde_json::to_string_pretty(checkpoint).map_err(|source| CheckpointError::Serialize {
            campaign_id: checkpoint.campaign_id,
            source,
        })?;

    fs::write(&tmp_path, body).map_err(|source| CheckpointError::WriteTempFailed {
        path: tmp_path.clone(),
        source,
    })?;

    fs::rename(&tmp_path, &final_path).map_err(|source| CheckpointError::RenameFailed {
        path: final_path.clone(),
        source,
    })?;

    Ok(())
}

/// Read the checkpoint for `campaign_id` under `roadmap_dir`. A missing
/// file returns [`ReadCheckpoint::Absent`] — never an error; a campaign
/// that has never been checkpointed (a first run, or one on its first
/// step) is the expected case, not a failure.
pub fn read_checkpoint(
    roadmap_dir: &Path,
    campaign_id: Uuid,
) -> Result<ReadCheckpoint, CheckpointError> {
    let path = checkpoint_path(roadmap_dir, campaign_id);

    let body = match fs::read_to_string(&path) {
        Ok(body) => body,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ReadCheckpoint::Absent);
        }
        Err(source) => {
            return Err(CheckpointError::ReadFailed { path, source });
        }
    };

    let checkpoint: Checkpoint = serde_json::from_str(&body)
        .map_err(|source| CheckpointError::Deserialize { path, source })?;

    Ok(ReadCheckpoint::Found(checkpoint))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(campaign_id: Uuid) -> Checkpoint {
        Checkpoint {
            campaign_id,
            steps: vec![
                CheckpointStep {
                    repo: "engine-rs".to_string(),
                    block_id: "EN.11.H".to_string(),
                    index: 1,
                    integrated: true,
                    branch: Some("EN.11.H-flow".to_string()),
                },
                CheckpointStep {
                    repo: "engine-rs".to_string(),
                    block_id: "EN.11.I".to_string(),
                    index: 2,
                    integrated: false,
                    branch: None,
                },
            ],
        }
    }

    #[test]
    fn checkpoint_round_trips_through_serde() {
        let campaign_id = Uuid::new_v4();
        let checkpoint = sample(campaign_id);

        let json = serde_json::to_string(&checkpoint).expect("serialize");
        let back: Checkpoint = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(back, checkpoint);
    }

    #[test]
    fn write_then_read_returns_an_equal_value() {
        let dir = tempfile::tempdir().expect("tempdir");
        let campaign_id = Uuid::new_v4();
        let checkpoint = sample(campaign_id);

        write_checkpoint(dir.path(), &checkpoint).expect("write");
        let read = read_checkpoint(dir.path(), campaign_id).expect("read");

        assert_eq!(read, ReadCheckpoint::Found(checkpoint));
    }

    #[test]
    fn reading_a_non_existent_checkpoint_path_returns_absent_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let campaign_id = Uuid::new_v4();

        let read = read_checkpoint(dir.path(), campaign_id).expect("read must not error");

        assert_eq!(read, ReadCheckpoint::Absent);
    }

    #[test]
    fn a_partially_written_file_is_never_observed_by_the_reader() {
        // Simulate a crash mid-write: only the temp file exists, the
        // final path does not. The reader must treat this exactly like
        // "no checkpoint" (Absent), never attempt to parse the temp file
        // or error on a torn read — because it never looks at the temp
        // path at all.
        let dir = tempfile::tempdir().expect("tempdir");
        let campaign_id = Uuid::new_v4();
        let checkpoint = sample(campaign_id);

        let final_path = checkpoint_path(dir.path(), campaign_id);
        let tmp_path = final_path.with_extension("json.tmp");
        // Write a deliberately truncated / torn body straight to the temp
        // path, mimicking a write that never got the chance to rename.
        let full_body = serde_json::to_string_pretty(&checkpoint).expect("serialize");
        let torn_body = &full_body[..full_body.len() / 2];
        fs::write(&tmp_path, torn_body).expect("write torn temp file");

        let read = read_checkpoint(dir.path(), campaign_id).expect("read must not error");

        assert_eq!(read, ReadCheckpoint::Absent);
    }

    #[test]
    fn write_checkpoint_leaves_no_temp_file_behind_on_success() {
        let dir = tempfile::tempdir().expect("tempdir");
        let campaign_id = Uuid::new_v4();
        let checkpoint = sample(campaign_id);

        write_checkpoint(dir.path(), &checkpoint).expect("write");

        let final_path = checkpoint_path(dir.path(), campaign_id);
        let tmp_path = final_path.with_extension("json.tmp");
        assert!(final_path.exists());
        assert!(!tmp_path.exists());
    }

    #[test]
    fn overwriting_an_existing_checkpoint_replaces_it_atomically() {
        let dir = tempfile::tempdir().expect("tempdir");
        let campaign_id = Uuid::new_v4();
        let mut checkpoint = sample(campaign_id);

        write_checkpoint(dir.path(), &checkpoint).expect("first write");

        checkpoint.steps.push(CheckpointStep {
            repo: "engine-rs".to_string(),
            block_id: "EN.11.J".to_string(),
            index: 3,
            integrated: true,
            branch: Some("EN.11.J-flow".to_string()),
        });
        write_checkpoint(dir.path(), &checkpoint).expect("second write");

        let read = read_checkpoint(dir.path(), campaign_id)
            .expect("read")
            .into_option()
            .expect("checkpoint should exist");
        assert_eq!(read.steps.len(), 3);
    }

    #[test]
    fn checkpoint_path_embeds_the_campaign_id_so_campaigns_never_collide() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        assert_ne!(
            checkpoint_path(dir.path(), a),
            checkpoint_path(dir.path(), b)
        );
    }
}
