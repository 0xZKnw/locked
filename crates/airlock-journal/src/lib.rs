//! Append-only, hash-chained receipt journal.
//!
//! The journal lives with the broker. The sandbox has no path to it — *that*,
//! not cryptography, is what stops an agent rewriting its own history. A hash
//! chain detects lazy tampering; it does not defend against someone who controls
//! the file. We rely on the file being out of the agent's reach, and we say so.
//!
//! The journal is also the replay cache: a resumed run reads prior receipts and
//! re-executes only what changed. That is why it is the project's central format
//! and not merely a log.

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
        input_tokens: u64,
        output_tokens: u64,
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
    /// Digest of every field above. Becomes the next receipt's `prev`.
    pub digest: String,
}

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
}

pub struct Chain {
    path: PathBuf,
    receipts: Vec<Receipt>,
}

impl Chain {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        let path = path.as_ref().to_path_buf();
        let receipts = if path.exists() {
            std::fs::read_to_string(&path)?
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(read_receipt)
                .collect::<Result<Vec<Receipt>, _>>()?
        } else {
            Vec::new()
        };
        let chain = Self { path, receipts };
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
            digest: String::new(),
        };
        receipt.digest = digest_of(&receipt)?;

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(file, "{}", serde_json::to_string(&receipt)?)?;
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
/// hash without circularity. Field order is serde's declaration order, which is
/// stable across runs of the same binary.
///
/// TODO before this is load-bearing: a real canonical form (sorted keys, fixed
/// number formatting), so a journal stays verifiable across serde versions.
fn digest_of(receipt: &Receipt) -> Result<String, JournalError> {
    let blanked = Receipt {
        digest: String::new(),
        ..receipt.clone()
    };
    let bytes = serde_json::to_vec(&blanked)?;
    Ok(format!("sha256:{}", hex::encode(Sha256::digest(&bytes))))
}

pub fn digest_bytes(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        std::env::temp_dir().join(format!("airlock-test-{}.jsonl", std::process::id()))
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

        // Reopening re-verifies from disk.
        let reopened = Chain::open(&path).unwrap();
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
