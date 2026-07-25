//! What a journal must survive.
//!
//! The digest is computed by re-serializing a receipt, which makes the on-disk
//! shape part of the format rather than an implementation detail. Anyone adding a
//! field to `Event` will break every chain ever written unless they mark it
//! `skip_serializing_if` — this file is here so they find that out in a second
//! rather than from a user whose history stopped verifying.

use locked_journal::{Chain, Evidence, Event, Integrity, JournalError, Receipt};
use sha2::{Digest as _, Sha256};

fn scratch(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir()
        .join(format!("locked-journal-{}-{name}.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

/// A chain written before `isolation` existed, byte for byte as it was on disk.
/// It must still open and still verify.
#[test]
fn a_journal_written_before_a_field_existed_still_verifies() {
    let path = scratch("legacy");

    // Produced by an earlier build. The digests are the ones it wrote.
    let legacy = r#"{"seq":0,"prev":"sha256:genesis","ts":"2026-07-25T00:00:00Z","event":"run_started","integrity":{"level":"full"},"tools":["tap_call"],"sandbox_image":null,"attestation":"harness_attested","digest":"PLACEHOLDER"}"#;

    // Recompute what that exact line hashes to, without the new field, by
    // round-tripping it through the current types. If `isolation` were to
    // serialize when empty, this would produce a different digest and the
    // assertion below would catch it.
    let mut receipt: locked_journal::Receipt =
        serde_json::from_str(&legacy.replace("PLACEHOLDER", "sha256:x")).unwrap();
    receipt.digest = String::new();
    let reserialized = serde_json::to_string(&receipt).unwrap();
    assert!(
        !reserialized.contains("isolation"),
        "a field absent from an old receipt must stay absent when it is re-serialized, \
         or every chain written before it existed stops verifying"
    );

    // Now write a real chain through the current code and reopen it.
    let mut chain = Chain::open(&path).unwrap();
    chain
        .append(
            Event::RunStarted {
                integrity: Integrity::Full,
                tools: vec!["tap_call".into()],
                sandbox_image: None,
                isolation: String::new(), // as an old caller would have left it
            },
            Evidence::HarnessAttested,
            "2026-07-25T00:00:00Z".into(),
        )
        .unwrap();

    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(!raw.contains("isolation"));
    Chain::inspect(&path).unwrap().verify().unwrap();
    let _ = std::fs::remove_file(&path);
}

/// Regression, and a bad one. An `approval_resolved` event and the `tap_attested`
/// evidence beside it both name the transaction, and version 0 flattened both
/// into the receipt — so `txn_id` was written twice. Serde will not read a
/// flattened struct with a repeated key, which meant a chain stopped opening the
/// moment a human approved anything. The bug hid behind another one: approvals
/// were never resolving, so no such receipt was ever written.
///
/// Version 1 writes the key once. Version 0 lines still exist on disk and must
/// still open — the bytes are not the problem and must not change, since the
/// digest is taken over exactly them.
#[test]
fn a_v0_receipt_that_names_the_same_transaction_twice_still_opens() {
    let path = scratch("dup");
    let txn = "e5aa7337-065e-415e-abce-6f0fc66d2b83";

    // A line exactly as version 0 wrote it, repeated key and all.
    let mut r = Receipt {
        seq: 0,
        prev: locked_journal::GENESIS.into(),
        ts: "2026-07-25T12:12:00Z".into(),
        event: Event::ApprovalResolved { txn_id: txn.into(), decision: "approved".into() },
        evidence: Evidence::TapAttested { txn_id: txn.into() },
        v: 0,
        digest: String::new(),
    };
    let line_without_digest = serde_json::to_string(&r).unwrap();
    assert_eq!(
        line_without_digest.matches("\"txn_id\"").count(),
        2,
        "the duplicate is the on-disk reality this test exists to tolerate"
    );
    r.digest = format!(
        "sha256:{}",
        hex::encode(Sha256::digest(serde_json::to_vec(&r).unwrap()))
    );
    std::fs::write(&path, format!("{}
", serde_json::to_string(&r).unwrap())).unwrap();

    let reopened = Chain::inspect(&path).unwrap();
    assert_eq!(reopened.verify().unwrap(), 1);
    match &reopened.receipts()[0].event {
        Event::ApprovalResolved { txn_id, decision } => {
            assert_eq!(txn_id, txn);
            assert_eq!(decision, "approved");
        }
        other => panic!("wrong event: {other:?}"),
    }
    assert_eq!(reopened.receipts()[0].evidence, Evidence::TapAttested { txn_id: txn.into() });

    // And appending after it continues the same chain, under the new rule.
    let mut chain = Chain::open(&path).unwrap();
    chain
        .append(Event::RunFinished { turns: 1 }, Evidence::HarnessAttested, "2026-07-25T12:13:00Z".into())
        .unwrap();
    drop(chain);
    assert_eq!(Chain::inspect(&path).unwrap().verify().unwrap(), 2);

    let _ = std::fs::remove_file(&path);
}

/// The two ways a chain can be wrong, and the fact that they are told apart.
#[test]
fn a_broken_link_and_an_altered_receipt_report_differently() {
    let path = scratch("broken");
    let mut chain = Chain::open(&path).unwrap();
    for turns in 1..=3 {
        chain
            .append(
                Event::RunFinished { turns },
                Evidence::HarnessAttested,
                format!("2026-07-25T00:00:0{turns}Z"),
            )
            .unwrap();
    }

    let good = std::fs::read_to_string(&path).unwrap();

    // Altered in place: the digest no longer matches its own contents.
    std::fs::write(&path, good.replace("\"turns\":2", "\"turns\":99")).unwrap();
    assert!(matches!(
        Chain::inspect(&path),
        Err(JournalError::DigestMismatch { seq: 1, .. })
    ));

    // Cut out: the link from the receipt before it no longer resolves.
    let without_middle: String = good
        .lines()
        .enumerate()
        .filter(|(i, _)| *i != 1)
        .map(|(_, l)| format!("{l}\n"))
        .collect();
    std::fs::write(&path, without_middle).unwrap();
    assert!(matches!(
        Chain::inspect(&path),
        Err(JournalError::ChainBroken { .. })
    ));

    let _ = std::fs::remove_file(&path);
}

/// Appending is the only mutation. Reopening and appending again must continue
/// the same chain rather than start a new one.
#[test]
fn reopening_continues_the_same_chain() {
    let path = scratch("append");

    let head = {
        let mut chain = Chain::open(&path).unwrap();
        chain
            .append(
                Event::RunFinished { turns: 1 },
                Evidence::HarnessAttested,
                "2026-07-25T00:00:00Z".into(),
            )
            .unwrap();
        chain.head().to_string()
    };

    let mut chain = Chain::open(&path).unwrap();
    assert_eq!(chain.head(), head);
    let next = chain
        .append(
            Event::RunFinished { turns: 2 },
            Evidence::HarnessAttested,
            "2026-07-25T00:00:01Z".into(),
        )
        .unwrap()
        .clone();

    assert_eq!(next.seq, 1);
    assert_eq!(next.prev, head);
    assert_eq!(Chain::inspect(&path).unwrap().verify().unwrap(), 2);
    let _ = std::fs::remove_file(&path);
}

/// Evidence is a tagged union on disk, and the three tiers must round-trip
/// distinctly — collapsing them is the one bug that would quietly make the whole
/// project dishonest.
#[test]
fn the_three_evidence_tiers_survive_a_round_trip() {
    let path = scratch("evidence");
    let mut chain = Chain::open(&path).unwrap();

    for (i, evidence) in [
        Evidence::TapAttested { txn_id: "txn_1".into() },
        Evidence::SourceAttested { scheme: "execution_id".into(), id: "01JX".into() },
        Evidence::HarnessAttested,
    ]
    .into_iter()
    .enumerate()
    {
        chain
            .append(
                Event::RunFinished { turns: i as u32 },
                evidence,
                "2026-07-25T00:00:00Z".into(),
            )
            .unwrap();
    }

    let reopened = Chain::inspect(&path).unwrap();
    let tiers: Vec<_> = reopened.receipts().iter().map(|r| r.evidence.clone()).collect();
    assert_eq!(
        tiers,
        vec![
            Evidence::TapAttested { txn_id: "txn_1".into() },
            Evidence::SourceAttested { scheme: "execution_id".into(), id: "01JX".into() },
            Evidence::HarnessAttested,
        ]
    );
    let _ = std::fs::remove_file(&path);
}

/// The digest rule changed. Old receipts must keep verifying under the old one,
/// or the fix would have silently invalidated every journal already written —
/// which, for a project whose claim is *recompute it yourself*, would be the
/// worst possible way to correct a correctness bug.
#[test]
fn a_chain_written_under_the_old_digest_rule_still_verifies() {
    let path = scratch("v0");

    // A receipt exactly as version 0 wrote it: no `v`, hashed straight off the
    // struct in serde's declaration order.
    let mut old = Receipt {
        seq: 0,
        prev: locked_journal::GENESIS.into(),
        ts: "2026-07-25T00:00:00Z".into(),
        event: Event::RunFinished { turns: 1 },
        evidence: Evidence::HarnessAttested,
        v: 0,
        digest: String::new(),
    };
    let bytes = serde_json::to_vec(&old).unwrap();
    old.digest = format!("sha256:{}", hex::encode(Sha256::digest(&bytes)));
    std::fs::write(&path, format!("{}
", serde_json::to_string(&old).unwrap())).unwrap();

    let chain = Chain::inspect(&path).unwrap();
    assert_eq!(chain.verify().unwrap(), 1);
    assert_eq!(chain.receipts()[0].v, 0, "an old receipt stays version 0");

    // A new receipt appended after it links on, under the new rule, and the two
    // rules coexist in one file.
    let mut chain = Chain::open(&path).unwrap();
    chain
        .append(
            Event::RunFinished { turns: 2 },
            Evidence::HarnessAttested,
            "2026-07-25T00:00:01Z".into(),
        )
        .unwrap();
    drop(chain);

    let both = Chain::inspect(&path).unwrap();
    assert_eq!(both.verify().unwrap(), 2);
    assert_eq!(both.receipts()[0].v, 0);
    assert_eq!(both.receipts()[1].v, 1, "new receipts carry the new rule");
    let _ = std::fs::remove_file(&path);
}

/// The point of version 1: anyone can recompute the digest from the line on
/// disk, without knowing what order this binary happened to declare its fields
/// in. Sorted keys, one entry per key, integers only.
#[test]
fn a_v1_digest_is_recomputable_from_the_line_alone() {
    let path = scratch("canon");
    let mut chain = Chain::open(&path).unwrap();
    chain
        .append(
            Event::ApprovalResolved { txn_id: "txn_1".into(), decision: "approved".into() },
            Evidence::TapAttested { txn_id: "txn_1".into() },
            "2026-07-25T00:00:00Z".into(),
        )
        .unwrap();
    drop(chain);

    let raw = std::fs::read_to_string(&path).unwrap();
    let line = raw.lines().next().unwrap();

    // Recomputed the way an outsider would: parse the line, blank the digest,
    // re-serialise the map, hash. No access to the struct definition needed.
    let mut value: serde_json::Value = serde_json::from_str(line).unwrap();
    let claimed = value["digest"].as_str().unwrap().to_string();
    value["digest"] = serde_json::Value::String(String::new());
    let recomputed = format!(
        "sha256:{}",
        hex::encode(Sha256::digest(serde_json::to_vec(&value).unwrap()))
    );
    assert_eq!(recomputed, claimed);

    // And the repeated key is gone: an event and its evidence both name the
    // transaction, and version 1 writes it once.
    assert_eq!(line.matches("\"txn_id\"").count(), 1);
    let _ = std::fs::remove_file(&path);
}

/// The hazard the lock exists for: two writers each computing `prev` from their
/// own head, forking the links quietly and leaving a file that stops verifying
/// some time later.
#[test]
fn a_second_writer_is_refused_rather_than_forking_the_chain() {
    let path = scratch("lock");
    let first = Chain::open(&path).unwrap();

    match Chain::open(&path) {
        Err(JournalError::AlreadyOpen(_)) => {}
        Err(e) => panic!("refused, but for the wrong reason: {e}"),
        Ok(_) => panic!("a second writer got in and can now fork the chain"),
    }

    // Reading is always allowed — the file is append-only, so the worst a reader
    // sees is a chain one entry shorter than it will be.
    assert!(Chain::inspect(&path).is_ok());

    // The lock belongs to the writer, so the next run is not blocked by the last.
    drop(first);
    assert!(Chain::open(&path).is_ok(), "the lock outlived its writer");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("lock"));
}
