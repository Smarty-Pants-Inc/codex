//! Proves which object at a synthetic mount target the helper created.
//!
//! bwrap needs an empty file or directory at each synthetic mount target. A
//! helper that is killed with SIGKILL never runs cleanup, so the target stays.
//! A pathname marker alone cannot show that the object now at that path is the
//! one the helper created. The helper therefore creates the target itself
//! before bwrap starts and records the identity of the new object. A later
//! cleanup removes the object only while it still matches that record, so a
//! real path that replaced it is kept.

use std::fs;
use std::fs::Metadata;
use std::fs::OpenOptions;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use crate::bwrap::SyntheticMountTarget;
use crate::bwrap::SyntheticMountTargetKind;

const CREATED_RECORD: &str = "created";

/// Creates a missing target and records the new object in `marker_dir`.
///
/// A path that already exists was not created here, so it gets no record. If
/// creation fails for another reason, no record is written and bwrap reports
/// the failure as before.
pub(crate) fn create_and_record(target: &SyntheticMountTarget, marker_dir: &Path) {
    let path = target.path();
    let created = match target.kind() {
        SyntheticMountTargetKind::EmptyFile => OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map(drop),
        SyntheticMountTargetKind::EmptyDirectory => fs::create_dir(path),
    };
    if created.is_err() {
        return;
    }
    // ponytail: if SIGKILL arrives between the create and the record write, the
    // target leaks without a record, as all targets did before. The filesystem
    // cannot create an object and write its record in one atomic step.
    let metadata = fs::symlink_metadata(path).unwrap_or_else(|err| {
        panic!(
            "failed to inspect created synthetic bubblewrap mount target {}: {err}",
            path.display()
        )
    });
    let record = marker_dir.join(CREATED_RECORD);
    fs::write(&record, identity(&metadata)).unwrap_or_else(|err| {
        panic!(
            "failed to record synthetic bubblewrap mount target {}: {err}",
            record.display()
        )
    });
}

/// Returns the recorded identity of the helper-created object, if any.
pub(crate) fn recorded_identity(marker_dir: &Path) -> Option<String> {
    let record = marker_dir.join(CREATED_RECORD);
    match fs::read_to_string(&record) {
        Ok(identity) => Some(identity),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => panic!(
            "failed to read synthetic bubblewrap mount target record {}: {err}",
            record.display()
        ),
    }
}

pub(crate) fn remove_record(marker_dir: &Path) {
    let record = marker_dir.join(CREATED_RECORD);
    match fs::remove_file(&record) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => panic!(
            "failed to remove synthetic bubblewrap mount target record {}: {err}",
            record.display()
        ),
    }
}

/// Identifies one object. The inode number can be reused after deletion, but
/// the new object also gets a new change time, and any change to the object
/// updates its change time.
pub(crate) fn identity(metadata: &Metadata) -> String {
    format!(
        "{} {} {} {}\n",
        metadata.dev(),
        metadata.ino(),
        metadata.ctime(),
        metadata.ctime_nsec()
    )
}
