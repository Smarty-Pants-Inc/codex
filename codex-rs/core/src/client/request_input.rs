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
        if self.observation_full_context && !self.client.state.provider.info().is_openai() {
            // build_responses_request applies this provider normalization too.
            // Apply it before capture so canonical equality and the bound input
            // match the actual sender, including non-kind warehouse metadata.
            for item in input.iter_mut() {
                item.clear_internal_chat_message_metadata_passthrough();
                if let ResponseItem::FunctionCall {
                    encrypted_function_args,
                    ..
                } = item
                {
                    *encrypted_function_args = None;
                }
            }
        }
        self.client.prepare_response_items_for_request(input);
    }
}
