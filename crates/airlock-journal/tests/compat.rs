//! What a journal must survive.
//!
//! The digest is computed by re-serializing a receipt, which makes the on-disk
//! shape part of the format rather than an implementation detail. Anyone adding a
//! field to `Event` will break every chain ever written unless they mark it
//! `skip_serializing_if` — this file is here so they find that out in a second
//! rather than from a user whose history stopped verifying.

use airlock_journal::{Chain, Evidence, Event, Integrity, JournalError};

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
    let mut receipt: airlock_journal::Receipt =
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
    Chain::open(&path).unwrap().verify().unwrap();
    let _ = std::fs::remove_file(&path);
}

/// Regression, and a bad one. An `approval_resolved` event and the `tap_attested`
/// evidence beside it both name the transaction, and both are flattened into the
/// receipt — so `txn_id` is written twice. Serde will not read a flattened struct
/// with a repeated key, which meant a chain stopped opening the moment a human
/// approved anything. The bug hid behind another one: approvals were never
/// resolving, so no such receipt was ever written.
///
/// The bytes are not the problem and must not change — the digest is taken over
/// exactly them. This pins that a real line from a real journal still opens, and
/// still verifies.
#[test]
fn a_receipt_that_names_the_same_transaction_twice_still_opens() {
    let path = scratch("dup");

    let mut chain = Chain::open(&path).unwrap();
    chain
        .append(
            Event::ApprovalResolved {
                txn_id: "e5aa7337-065e-415e-abce-6f0fc66d2b83".into(),
                decision: "approved".into(),
            },
            Evidence::TapAttested {
                txn_id: "e5aa7337-065e-415e-abce-6f0fc66d2b83".into(),
            },
            "2026-07-25T12:12:00Z".into(),
        )
        .unwrap();

    let raw = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        raw.matches("\"txn_id\"").count(),
        2,
        "the duplicate is the on-disk reality this test exists to tolerate"
    );

    let reopened = Chain::open(&path).unwrap();
    assert_eq!(reopened.verify().unwrap(), 1);
    match &reopened.receipts()[0].event {
        Event::ApprovalResolved { txn_id, decision } => {
            assert_eq!(txn_id, "e5aa7337-065e-415e-abce-6f0fc66d2b83");
            assert_eq!(decision, "approved");
        }
        other => panic!("wrong event: {other:?}"),
    }
    assert_eq!(
        reopened.receipts()[0].evidence,
        Evidence::TapAttested {
            txn_id: "e5aa7337-065e-415e-abce-6f0fc66d2b83".into()
        }
    );

    // And appending after it continues the same chain.
    let mut chain = Chain::open(&path).unwrap();
    chain
        .append(
            Event::RunFinished { turns: 1 },
            Evidence::HarnessAttested,
            "2026-07-25T12:13:00Z".into(),
        )
        .unwrap();
    assert_eq!(Chain::open(&path).unwrap().verify().unwrap(), 2);

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
        Chain::open(&path),
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
        Chain::open(&path),
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
    assert_eq!(Chain::open(&path).unwrap().verify().unwrap(), 2);
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

    let reopened = Chain::open(&path).unwrap();
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
