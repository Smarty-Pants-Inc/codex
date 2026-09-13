//! Read existing durable speech facts without changing the remote session.

use super::*;
use crate::realtime_history::RealtimeHistory;
use codex_app_server_protocol::ThreadTimelineListParams;
use codex_app_server_protocol::ThreadTimelineListResponse;

impl AppServerSession {
    pub(super) async fn realtime_history(&mut self, thread_id: ThreadId) -> RealtimeHistory {
        let result = tokio::time::timeout(std::time::Duration::from_secs(/*secs*/ 5), async {
            let mut pages = Vec::new();
            let mut cursor = None;
            // Match the server's maximum page size; bound both work and retained history.
            for _ in 0..5 {
                let request_id = self.next_request_id();
                let page: ThreadTimelineListResponse = self
                    .client
                    .request_typed(ClientRequest::ThreadTimelineList {
                        request_id,
                        params: ThreadTimelineListParams {
                            thread_id: thread_id.to_string(),
                            cursor: cursor.clone(),
                            limit: Some(100),
                        },
                    })
                    .await?;
                pages.push(page.data);
                if page.next_cursor.is_none() {
                    cursor = None;
                    break;
                }
                if page.next_cursor == cursor {
                    break;
                }
                cursor = page.next_cursor;
            }
            let mut history =
                RealtimeHistory::from_timeline(pages.into_iter().rev().flatten().collect());
            if cursor.is_some() && !history.items.is_empty() {
                history.notice =
                    Some("Voice replay is limited to the latest 500 timeline entries.".into());
            }
            Ok::<_, codex_app_server_client::TypedRequestError>(history)
        })
        .await;
        match result {
            Ok(Ok(history)) => history,
            Ok(Err(_)) | Err(_) => RealtimeHistory {
                notice: Some(
                    "Voice history is unavailable; only newly received speech will be shown."
                        .into(),
                ),
                ..Default::default()
            },
        }
    }
}
