use super::GoalStore;
use codex_protocol::ThreadId;
use codex_protocol::turn_input::TurnStartGuard;

#[cfg(test)]
#[path = "keep_working_tests.rs"]
mod tests;

impl GoalStore {
    /// Reads the thread's opt-in setting, independently of any legacy goal.
    pub async fn keep_working_enabled(&self, thread_id: ThreadId) -> anyhow::Result<bool> {
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM thread_keep_working WHERE thread_id = ?)")
            .bind(thread_id.to_string())
            .fetch_one(self.pool.as_ref())
            .await
            .map_err(Into::into)
    }

    /// Persists the setting only; this never starts or replays a turn.
    pub async fn set_keep_working(&self, thread_id: ThreadId, enabled: bool) -> anyhow::Result<()> {
        self.set_keep_working_inner(thread_id, enabled, /*guard*/ None)
            .await
    }

    /// Fence a runtime-owned write before commit while SQLite holds its writer
    /// lock. A superseded operation must not overwrite a newer owner's setting.
    pub async fn set_keep_working_if_current(
        &self,
        thread_id: ThreadId,
        enabled: bool,
        guard: &TurnStartGuard,
    ) -> anyhow::Result<()> {
        self.set_keep_working_inner(thread_id, enabled, Some(guard))
            .await
    }

    async fn set_keep_working_inner(
        &self,
        thread_id: ThreadId,
        enabled: bool,
        guard: Option<&TurnStartGuard>,
    ) -> anyhow::Result<()> {
        let query = if enabled {
            "INSERT INTO thread_keep_working (thread_id) VALUES (?) ON CONFLICT DO NOTHING"
        } else {
            "DELETE FROM thread_keep_working WHERE thread_id = ?"
        };
        // A cancelled writer must roll back queued work, not leave an autocommit
        // INSERT that could run after a later stop's DELETE.
        let mut transaction = self.pool.begin().await?;
        sqlx::query(query)
            .bind(thread_id.to_string())
            .execute(&mut *transaction)
            .await?;
        anyhow::ensure!(
            !guard.is_some_and(TurnStartGuard::is_revoked),
            "keep_working intent was stopped or superseded"
        );
        transaction.commit().await?;
        Ok(())
    }
}
