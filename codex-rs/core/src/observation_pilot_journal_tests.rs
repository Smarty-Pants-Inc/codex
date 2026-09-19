use super::*;
use pretty_assertions::assert_eq;
use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;

// This is only a file-writer fixture. It cannot construct a count completion or
// final request capability, and is never evidence of provider authentication.
fn record() -> CountJournalRecord {
    CountJournalRecord::Complete(CountComplete {
        version: 1,
        kind: "complete",
        operation: 1,
        decision_id: "fixture-decision".into(),
        attempt_id: "fixture-attempt".into(),
        request_id: "fixture-request".into(),
        input_tokens: "7".into(),
        response_sha256: "a".repeat(/*n*/ 64),
    })
}

#[test]
fn original_held_journal_refuses_alias_nonempty_and_nonappend() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let inputs: Vec<_> = (0..5)
        .map(|_| tempfile::tempfile())
        .collect::<Result<_, _>>()?;
    let others = [&inputs[0], &inputs[1], &inputs[2], &inputs[3], &inputs[4]];
    let path = home.path().join("ledger");
    let file = OpenOptions::new()
        .create_new(true)
        .append(true)
        .read(true)
        .mode(0o600)
        .open(&path)?;
    let identity = held_identity(&file)?;
    let alias = file.try_clone()?;
    let aliases = [&alias, &inputs[1], &inputs[2], &inputs[3], &inputs[4]];
    assert!(matches!(
        PilotCountJournal::receive(file, identity, aliases),
        Err(PilotAuthorityError::Denied)
    ));

    let file = OpenOptions::new().write(true).open(&path)?;
    assert!(matches!(
        PilotCountJournal::receive(file, identity, others),
        Err(PilotAuthorityError::Denied)
    ));
    std::fs::write(&path, b"prior uncertain state\n")?;
    let file = OpenOptions::new().append(true).open(&path)?;
    assert!(matches!(
        PilotCountJournal::receive(file, identity, others),
        Err(PilotAuthorityError::Denied)
    ));
    assert_eq!(std::fs::read(&path)?, b"prior uncertain state\n");
    Ok(())
}

#[test]
fn durable_append_preserves_exact_bytes_and_failure_is_sticky() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let inputs: Vec<_> = (0..5)
        .map(|_| tempfile::tempfile())
        .collect::<Result<_, _>>()?;
    let path = home.path().join("ledger");
    let file = OpenOptions::new()
        .create_new(true)
        .append(true)
        .read(true)
        .mode(0o600)
        .open(&path)?;
    let identity = held_identity(&file)?;
    let journal = PilotCountJournal::receive(
        file,
        identity,
        [&inputs[0], &inputs[1], &inputs[2], &inputs[3], &inputs[4]],
    )?;
    let entry = record();
    journal.append(&entry)?;
    let mut expected = serde_json::to_vec(&entry)?;
    expected.push(b'\n');
    assert_eq!(
        (std::fs::read(&path)?, journal.failed()),
        (expected.clone(), false)
    );

    // Simulate an unsafe held-file change without depending on root/non-root
    // write permissions. Restoring the mode must not restore the failed handle.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(/*mode*/ 0o400))?;
    assert_eq!(journal.append(&entry), Err(PilotAuthorityError::Denied));
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(/*mode*/ 0o600))?;
    assert_eq!(
        journal.append(&entry),
        Err(PilotAuthorityError::Unavailable)
    );
    assert_eq!((std::fs::read(&path)?, journal.failed()), (expected, true));
    Ok(())
}

#[test]
fn unexpected_external_append_fences_without_truncating_evidence() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let inputs: Vec<_> = (0..5)
        .map(|_| tempfile::tempfile())
        .collect::<Result<_, _>>()?;
    let path = home.path().join("ledger");
    let file = OpenOptions::new()
        .create_new(true)
        .append(true)
        .read(true)
        .mode(0o600)
        .open(&path)?;
    let identity = held_identity(&file)?;
    let journal = PilotCountJournal::receive(
        file,
        identity,
        [&inputs[0], &inputs[1], &inputs[2], &inputs[3], &inputs[4]],
    )?;
    let mut outside = OpenOptions::new().append(true).open(&path)?;
    outside.write_all(b"foreign writer\n")?;
    assert_eq!(journal.append(&record()), Err(PilotAuthorityError::Denied));
    assert_eq!(
        (std::fs::read(&path)?, journal.failed()),
        (b"foreign writer\n".to_vec(), true)
    );
    Ok(())
}
