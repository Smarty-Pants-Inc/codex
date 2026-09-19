use super::ModelClient;
use super::ModelClientSession;
use codex_protocol::models::ResponseItem;

impl ModelClient {
    pub(crate) fn prepare_response_items_for_request(&self, input: &mut [ResponseItem]) {
        for item in input {
            if item.id().is_some_and(|id| !id.is_prefixed()) {
                item.set_id(/*new_id*/ None);
            }
            if !self.state.content_item_kinds_enabled {
                item.clear_content_item_kinds();
            }
        }
    }
}

impl ModelClientSession {
    /// Use the sender's normalization for observation decision equality too.
    pub(crate) fn prepare_response_items_for_request(&self, input: &mut [ResponseItem]) {
        self.client.prepare_response_items_for_request(input);
    }
}
