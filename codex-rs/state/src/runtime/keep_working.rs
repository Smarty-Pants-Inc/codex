use super::GoalStore;
use codex_protocol::ThreadId;

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
        transaction.commit().await?;
        Ok(())
    }
}
