//! Append-only, hash-chained receipt journal.
//!
//! The journal lives with the broker. The sandbox has no path to it — *that*,
//! not cryptography, is what stops an agent rewriting its own history. A hash
//! chain detects lazy tampering; it does not defend against someone who controls
//! the file. We rely on the file being out of the agent's reach, and we say so.
//!
//! It is deliberately not a transcript store. Prompts, responses and file
//! contents appear only as digests, so a journal can be handed to someone who
//! should see *what happened* without seeing what was said. That is also why it
//! cannot be replayed *from*: there is nothing in here to replay, by design. The
//! conversation lives beside it, in a file that carries no attestation.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// Who can corroborate a receipt.
///
/// This distinction is the honest core of the project. A TAP-gated write is
/// witnessed by a third party *and* a human. A read is witnessed by nobody but
/// this file. Rendering the two identically would be a lie, so the type system
/// refuses to let a caller forget which one it has.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "attestation", rename_all = "snake_case")]
pub enum Evidence {
    /// TAP holds a transaction record and a human approved it.
    /// Verifiable by someone who does not trust this machine.
    TapAttested { txn_id: String },
    /// The upstream returned its own verifiable identifier — a Dune
    /// `execution_id`, an ETag, a block number. Weaker than TAP, far
    /// stronger than nothing, and free when the source offers it.
    SourceAttested { scheme: String, id: String },
    /// Only this journal says so.
    HarnessAttested,
}

/// Whether the run held the egress invariant.
///
/// A run built with the `direct-llm` escape hatch, or with any other relaxation,
/// records that fact in its own chain. The artifact declares its own integrity
/// rather than leaving the reader to assume.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "level", rename_all = "snake_case")]
pub enum Integrity {
    /// Every byte of third-party egress went through TAP.
    Full,
    Degraded { reason: String },
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    RunStarted {
        integrity: Integrity,
        tools: Vec<String>,
        sandbox_image: Option<String>,
        /// What the sandbox actually guaranteed — `container:<image>`,
        /// `workspace`, or `none`. The tool list already implies the surface;
        /// this states the strength behind it, which a reader cannot infer.
        ///
        /// `skip_serializing_if` is load-bearing, not tidiness: `digest_of`
        /// re-serializes a receipt to check it, so a field that appeared in the
        /// output of an older receipt would change its bytes and break every
        /// chain written before this existed.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        isolation: String,
    },
    /// An inference. Prompt and response are stored as digests, never verbatim:
    /// the journal is an audit trail, not a transcript store, and the prompt can
    /// carry anything the agent has read.
    Inference {
        model: String,
        prompt_digest: String,
        response_digest: String,
        /// Everything the model read, cached or not — the size of the context,
        /// not the size of the bill.
        input_tokens: u64,
        /// How much of that came back from the provider's cache. Absent on
        /// receipts written before caching existed, and on turns that read
        /// nothing from it, so old chains keep their exact bytes and their
        /// digests.
        #[serde(default, skip_serializing_if = "is_zero_u64")]
        cached_tokens: u64,
        output_tokens: u64,
    },
    /// The conversation was shortened to fit the model's window.
    ///
    /// Recorded because it is a loss. Everything after this point was reasoned
    /// about from a summary rather than from what was actually said, and a reader
    /// deciding how much to trust a late answer needs to know that — which is why
    /// both digests are here: the conversation that went in and the one that came
    /// out are each pinned, so the shortening is visible even though the text it
    /// dropped is not stored anywhere.
    ConversationCompacted {
        /// Messages folded into the summary.
        dropped: u32,
        /// Messages carried forward verbatim.
        kept: u32,
        before_digest: String,
        after_digest: String,
    },
    TapCall {
        credential: String,
        /// Host only. The full target can carry query parameters we have no
        /// business persisting.
        target_host: String,
        method: String,
        upstream_status: Option<u16>,
    },
    ApprovalResolved {
        txn_id: String,
        decision: String,
    },
    SandboxCall {
        tool: String,
        args_digest: String,
        result_digest: String,
        exit_code: Option<i32>,
    },
    RunFinished {
        turns: u32,
    },
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Receipt {
    pub seq: u64,
    /// Digest of the preceding receipt, or `GENESIS` for the first.
    pub prev: String,
    pub ts: String,
    #[serde(flatten)]
    pub event: Event,
    #[serde(flatten)]
    pub evidence: Evidence,
    /// Which rule computed this receipt's digest.
    ///
    /// Absent means version 0: the digest was taken over serde's field order,
    /// which is only stable for one build of one binary. Version 1 is the
    /// canonical form below, which anyone can recompute.
    ///
    /// The field exists so old chains keep verifying instead of being silently
    /// invalidated by the fix. A project whose claim is "recompute it yourself"
    /// cannot rewrite the rule and hope nobody had a journal.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub v: u8,
    /// Digest of every field above. Becomes the next receipt's `prev`.
    pub digest: String,
}

fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}

fn is_zero(v: &u8) -> bool {
    *v == 0
}

/// The digest rule new receipts are written under.
pub const DIGEST_VERSION: u8 = 1;

pub const GENESIS: &str = "sha256:genesis";

#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("chain broken at seq {seq}: prev is {found}, expected {expected}")]
    ChainBroken {
        seq: u64,
        found: String,
        expected: String,
    },
    #[error("receipt {seq} has been altered: digest is {found}, recomputes to {expected}")]
    DigestMismatch {
        seq: u64,
        found: String,
        expected: String,
    },
    #[error("{0} is already open for appending — another writer holds it")]
    AlreadyOpen(String),
}

pub struct Chain {
    path: PathBuf,
    receipts: Vec<Receipt>,
    /// Held for as long as this handle can append. `None` for a read-only view.
    lock: Option<PathBuf>,
}

/// Released when the writer is dropped, including on a panic — so a crash costs
/// a stale lock file at worst, which the next `open` reports rather than
/// silently stepping over.
impl Drop for Chain {
    fn drop(&mut self) {
        if let Some(lock) = &self.lock {
            let _ = std::fs::remove_file(lock);
        }
    }
}

impl Chain {
    /// Open for appending, exclusively.
    ///
    /// Two handles on one chain is not a race that shows up as a crash: each
    /// computes `prev` from its own in-memory head, so the second writer forks
    /// the links and the file stops verifying — quietly, after the fact. The
    /// only prevention used to be that the window declined to poll during a run,
    /// which is a convention, not a guarantee.
    ///
    /// The lock is a file created with `create_new`, which is atomic at the
    /// filesystem: whoever wins, wins. Advisory in the sense that a process that
    /// ignores it can still write — but every writer here goes through `Chain`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        let path = path.as_ref().to_path_buf();
        let lock = path.with_extension("lock");

        match std::fs::OpenOptions::new().write(true).create_new(true).open(&lock) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(JournalError::AlreadyOpen(path.display().to_string()));
            }
            Err(e) => return Err(e.into()),
        }

        let mut chain = Self::load(path)?;
        chain.lock = Some(lock);
        Ok(chain)
    }

    /// Read a chain without claiming it.
    ///
    /// Reading is always safe — the file is append-only, so the worst a reader
    /// sees is a chain one entry shorter than it will be. Making a reader take
    /// the lock would mean opening a session in the window could block a run.
    pub fn inspect(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        Self::load(path.as_ref().to_path_buf())
    }

    fn load(path: PathBuf) -> Result<Self, JournalError> {
        let receipts = if path.exists() {
            std::fs::read_to_string(&path)?
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(read_receipt)
                .collect::<Result<Vec<Receipt>, _>>()?
        } else {
            Vec::new()
        };
        let chain = Self { path, receipts, lock: None };
        chain.verify()?;
        Ok(chain)
    }

    pub fn head(&self) -> &str {
        self.receipts.last().map_or(GENESIS, |r| r.digest.as_str())
    }

    pub fn receipts(&self) -> &[Receipt] {
        &self.receipts
    }

    pub fn append(
        &mut self,
        event: Event,
        evidence: Evidence,
        now: String,
    ) -> Result<&Receipt, JournalError> {
        let mut receipt = Receipt {
            seq: self.receipts.len() as u64,
            prev: self.head().to_string(),
            ts: now,
            event,
            evidence,
            v: DIGEST_VERSION,
            digest: String::new(),
        };
        receipt.digest = digest_of(&receipt)?;

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        // Written through `Value` for the same reason it is hashed that way: one
        // entry per key, so the line on disk is what the digest was taken over.
        writeln!(file, "{}", serde_json::to_value(&receipt)?)?;
        file.sync_all()?;

        self.receipts.push(receipt);
        Ok(self.receipts.last().expect("just pushed"))
    }

    /// Recompute every digest and walk the `prev` links.
    ///
    /// Proves the file is internally consistent. It does *not* prove the file is
    /// true — see the module docs.
    pub fn verify(&self) -> Result<u64, JournalError> {
        let mut expected_prev = GENESIS.to_string();
        for receipt in &self.receipts {
            if receipt.prev != expected_prev {
                return Err(JournalError::ChainBroken {
                    seq: receipt.seq,
                    found: receipt.prev.clone(),
                    expected: expected_prev,
                });
            }
            let recomputed = digest_of(receipt)?;
            if recomputed != receipt.digest {
                return Err(JournalError::DigestMismatch {
                    seq: receipt.seq,
                    found: receipt.digest.clone(),
                    expected: recomputed,
                });
            }
            expected_prev = receipt.digest.clone();
        }
        Ok(self.receipts.len() as u64)
    }
}

/// Parse one line of the journal.
///
/// Going through `Value` first is not a stylistic choice. `Receipt` flattens both
/// the event and the evidence, and two variants legitimately carry the same field
/// name — an `approval_resolved` event and the `tap_attested` evidence beside it
/// both name the transaction. Serialization writes `txn_id` twice; serde then
/// refuses to *read* a flattened struct with a repeated key, so a chain became
/// unopenable the moment a human approved anything.
///
/// Parsing to `Value` collapses the repeat (both copies hold the same string, so
/// nothing is lost), and the struct is rebuilt from that.
///
/// The alternative — renaming one of the fields — would change what those
/// receipts serialize to, and the digest is computed over exactly that. Every
/// approval already written would stop verifying. The file on disk is correct;
/// it was the reader that was wrong, so the reader is what changed.
fn read_receipt(line: &str) -> Result<Receipt, serde_json::Error> {
    let value: serde_json::Value = serde_json::from_str(line)?;
    serde_json::from_value(value)
}

/// Digest over the receipt with `digest` blanked, so the field can carry its own
/// hash without circularity.
///
/// Two rules, chosen by the receipt's own `v`:
///
/// **v1 — canonical.** The receipt is turned into a JSON object first, then
/// hashed. That does three things at once. Keys come out sorted, because
/// `serde_json`'s map is a `BTreeMap` — so the hash no longer depends on the
/// order fields happen to be declared in, and reordering a struct or upgrading
/// serde can no longer invalidate a journal. The two flattened halves that both
/// name `txn_id` collapse to one entry, so the bytes are a well-formed object
/// rather than one with a repeated key. And every number in a receipt is an
/// integer, which JSON writes exactly, so there is no float formatting to pin.
///
/// **v0 — legacy.** Receipts written before this rule existed are hashed the way
/// they were written: serde's declaration order, straight off the struct. They
/// keep verifying. Silently changing the rule would have invalidated every
/// journal already on disk, which for a project whose claim is *recompute it
/// yourself* would be the worst possible way to fix a correctness bug.
fn digest_of(receipt: &Receipt) -> Result<String, JournalError> {
    let blanked = Receipt {
        digest: String::new(),
        ..receipt.clone()
    };
    let bytes = if blanked.v >= 1 {
        serde_json::to_vec(&serde_json::to_value(&blanked)?)?
    } else {
        serde_json::to_vec(&blanked)?
    };
    Ok(format!("sha256:{}", hex::encode(Sha256::digest(&bytes))))
}

pub fn digest_bytes(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        std::env::temp_dir().join(format!("locked-test-{}.jsonl", std::process::id()))
    }

    #[test]
    fn chain_links_and_verifies() {
        let path = tmp();
        let _ = std::fs::remove_file(&path);
        let mut chain = Chain::open(&path).unwrap();

        chain
            .append(
                Event::RunStarted {
                    integrity: Integrity::Full,
                    tools: vec!["tap_call".into()],
                    sandbox_image: None,
                    isolation: "workspace".into(),
                },
                Evidence::HarnessAttested,
                "2026-07-25T00:00:00Z".into(),
            )
            .unwrap();
        let first = chain.head().to_string();

        chain
            .append(
                Event::TapCall {
                    credential: "dune".into(),
                    target_host: "api.dune.com".into(),
                    method: "GET".into(),
                    upstream_status: Some(200),
                },
                Evidence::HarnessAttested,
                "2026-07-25T00:00:01Z".into(),
            )
            .unwrap();

        assert_eq!(chain.receipts()[1].prev, first);
        assert_eq!(chain.verify().unwrap(), 2);

        // Reopening re-verifies from disk. A read, so it does not take the lock
        // the writer above is still holding.
        let reopened = Chain::inspect(&path).unwrap();
        assert_eq!(reopened.receipts().len(), 2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn tampering_is_detected() {
        let path = tmp().with_extension("tamper.jsonl");
        let _ = std::fs::remove_file(&path);
        let mut chain = Chain::open(&path).unwrap();
        chain
            .append(
                Event::RunFinished { turns: 3 },
                Evidence::HarnessAttested,
                "2026-07-25T00:00:00Z".into(),
            )
            .unwrap();

        // Rewrite the event without recomputing the digest — the lazy tamper.
        chain.receipts[0].event = Event::RunFinished { turns: 99 };
        assert!(matches!(
            chain.verify(),
            Err(JournalError::DigestMismatch { .. })
        ));
        let _ = std::fs::remove_file(&path);
    }
}
