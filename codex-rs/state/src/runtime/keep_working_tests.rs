use crate::SqliteConfig;
use crate::StateRuntime;
use crate::runtime::test_support::test_thread_metadata;
use crate::runtime::test_support::unique_temp_dir;
use codex_protocol::ThreadId;
use codex_utils_absolute_path::test_support::PathExt;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn keep_working_is_thread_scoped_persistent_and_deleted_with_thread() -> anyhow::Result<()> {
    let home = unique_temp_dir();
    let _cleanup = scopeguard::guard(home.clone(), |path| {
        let _ = std::fs::remove_dir_all(path);
    });
    let sqlite = SqliteConfig::new_for_testing(home.as_path().abs());
    let runtime = StateRuntime::init(sqlite.clone(), "test-provider".to_string()).await?;
    let thread_id = ThreadId::new();
    let other = ThreadId::new();
    runtime
        .upsert_thread(&test_thread_metadata(&home, thread_id, home.clone()))
        .await?;
    assert!(
        !runtime
            .thread_goals()
            .keep_working_enabled(thread_id)
            .await?
    );
    for enabled in [true, true, false, true] {
        runtime
            .thread_goals()
            .set_keep_working(thread_id, enabled)
            .await?;
        assert_eq!(
            runtime
                .thread_goals()
                .keep_working_enabled(thread_id)
                .await?,
            enabled
        );
    }
    runtime.close().await;
    let runtime = StateRuntime::init(sqlite, "test-provider".to_string()).await?;
    assert_eq!(
        (
            runtime
                .thread_goals()
                .keep_working_enabled(thread_id)
                .await?,
            runtime.thread_goals().keep_working_enabled(other).await?
        ),
        (true, false),
    );
    runtime.delete_threads_strict(&[thread_id]).await?;
    assert!(
        !runtime
            .thread_goals()
            .keep_working_enabled(thread_id)
            .await?
    );
    runtime.close().await;
    Ok(())
}
