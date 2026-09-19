//! Original-owner async count, durable completion and single-use final send.
use super::*;
use crate::observation::ObservationAttempt;
use crate::observation::ObservationSlot;
use crate::observation::ObservationTransportLease;
use codex_http_client::SingleAttemptTransport;
use codex_http_client::StreamResponse;
use futures::StreamExt;
use std::sync::Mutex;

struct CountResult {
    plan: CountPlan,
    input_tokens: u64,
    response_sha256: String,
}

/// Not Clone and not constructible from a parsed integer or imported journal.
struct FinalRequestReceipt(CountResult);
struct ReceiptCell(Mutex<Option<FinalRequestReceipt>>);

/// Cancellation/uncertain outcomes retire this allocation without releasing debt.
/// The lease survives detached blocking work, so shutdown still waits for fsync.
struct OperationGuard {
    lease: Arc<ObservationTransportLease>,
    successful: bool,
}
impl Drop for OperationGuard {
    fn drop(&mut self) {
        if !self.successful
            && let Ok(mut state) = self.lease.slot().state.lock()
            && let Some(ledger) = state.pilot.as_mut()
        {
            ledger.revoke();
        }
    }
}

impl ObservationSlot {
    pub(crate) fn native_count_output_limit(
        &self,
    ) -> Result<Option<NonZeroU64>, PilotAuthorityError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| PilotAuthorityError::Unavailable)?;
        if state.revoked {
            return Err(PilotAuthorityError::Expired);
        }
        let Some(ledger) = state.pilot.as_mut() else {
            return Ok(None);
        };
        ledger.expired |= ledger.issuer.recheck_grant(&ledger.claims).is_err()
            || ledger
                .count_journal
                .as_ref()
                .is_some_and(|journal| journal.failed());
        if ledger.expired {
            return Err(PilotAuthorityError::Expired);
        }
        Ok(ledger
            .issuer
            .count_scope(&ledger.claims)?
            .map(|scope| scope.output_tokens))
    }

    /// The only async producer. No state/journal mutex crosses an await. All
    /// transport constructors are fixed-route/no-resend, not caller attestations.
    pub(crate) async fn stream_counted_request(
        self: &Arc<Self>,
        decision_id: Uuid,
        inference: &Request,
    ) -> Result<(StreamResponse, ObservationAttempt), PilotAuthorityError> {
        let lease = Arc::clone(self).transport_lease();
        let guard = OperationGuard {
            lease: Arc::clone(&lease),
            successful: false,
        };
        let wire = inference
            .extensions
            .get::<Arc<CountWire>>()
            .ok_or(PilotAuthorityError::Unavailable)?
            .clone();
        let plan = self
            .prepare_count_attempt(decision_id, inference, wire)
            .map_err(|_| PilotAuthorityError::Denied)?;
        // Move the guard INTO blocking work. If the awaiting task disappears,
        // the durable writer remains owned, then fences when its result drops.
        let (durable, mut guard) =
            tokio::task::spawn_blocking(move || plan.persist().map(|plan| (plan, guard)))
                .await
                .map_err(|_| PilotAuthorityError::Unavailable)??;
        self.acknowledge_count_debit(&durable)?;
        let mut plan = durable.original;
        let count_request = std::mem::replace(
            &mut plan.count_request,
            Request::new(http::Method::POST, String::new()),
        );
        let response_bytes = tokio::time::timeout_at(plan.deadline.into(), async {
            let mut response = send_once(
                &plan.factory,
                &plan.scope.count_url,
                count_request,
                |request| {
                    self.recheck_count_plan(&plan, request, CountBoundary::Count)
                        .map_err(|_| "native count admission unavailable".to_owned())
                },
            )
            .await
            .map_err(|_| PilotAuthorityError::Unavailable)?;
            if response.status != http::StatusCode::OK
                || response
                    .headers
                    .contains_key(http::header::CONTENT_ENCODING)
            {
                return Err(PilotAuthorityError::Denied);
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response.bytes.next().await {
                let chunk = chunk.map_err(|_| PilotAuthorityError::Unavailable)?;
                if bytes
                    .len()
                    .checked_add(chunk.len())
                    .is_none_or(|size| size > 4096)
                {
                    return Err(PilotAuthorityError::Denied);
                }
                bytes.extend_from_slice(&chunk);
            }
            // EOF is necessary, not sufficient: original authenticated send and
            // complete bounded body consumption are owned by this operation.
            Ok(bytes)
        })
        .await
        .map_err(|_| PilotAuthorityError::Expired)??;
        let input_tokens = codex_api::parse_response_count(&response_bytes)
            .map_err(|_| PilotAuthorityError::Denied)?;
        if input_tokens
            .checked_add(plan.scope.output_tokens.get())
            .is_none_or(|total| total > plan.scope.context_tokens.get())
        {
            return Err(PilotAuthorityError::Exhausted);
        }
        self.recheck_count_plan(&plan, inference, CountBoundary::Inference)?;
        let result = CountResult {
            plan,
            input_tokens,
            response_sha256: format!("{:x}", Sha256::digest(&response_bytes)),
        };
        let (result, returned_guard) = tokio::task::spawn_blocking(move || {
            let record = super::super::journal::CountJournalRecord::Complete(
                super::super::journal::CountComplete {
                    version: 1,
                    kind: "complete",
                    operation: (result.plan.operation + 1) as u8,
                    decision_id: result.plan.decision_id.to_string(),
                    attempt_id: result.plan.attempt_id.to_string(),
                    request_id: result.plan.request_id.to_string(),
                    input_tokens: result.input_tokens.to_string(),
                    response_sha256: result.response_sha256.clone(),
                },
            );
            result
                .plan
                .journal
                .append(&record)
                .map(|()| (result, guard))
        })
        .await
        .map_err(|_| PilotAuthorityError::Unavailable)??;
        guard = returned_guard;
        self.recheck_count_plan(&result.plan, inference, CountBoundary::Inference)?;
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| PilotAuthorityError::Unavailable)?;
            let ledger = state
                .pilot
                .as_mut()
                .ok_or(PilotAuthorityError::Unavailable)?;
            let pending = ledger
                .pending_counts
                .get_mut(result.plan.operation)
                .ok_or(PilotAuthorityError::Denied)?;
            if !Arc::ptr_eq(&pending.brand, &result.plan.brand)
                || pending.completed
                || pending.consumed
            {
                return Err(PilotAuthorityError::Replay);
            }
            pending.completed = true;
            pending.input_tokens = Some(result.input_tokens);
        }
        let factory = result.plan.factory.clone();
        let inference_url = result.plan.scope.inference_url.clone();
        let attempt = ObservationAttempt {
            decision_id: result.plan.decision_id,
            attempt_id: result.plan.attempt_id,
        };
        let deadline = result.plan.deadline;
        let receipt = ReceiptCell(Mutex::new(Some(FinalRequestReceipt(result))));
        let mut response = tokio::time::timeout_at(
            deadline.into(),
            send_once(&factory, &inference_url, inference.clone(), |actual| {
                let FinalRequestReceipt(result) = receipt
                    .0
                    .lock()
                    .map_err(|_| "native receipt unavailable".to_owned())?
                    .take()
                    .ok_or_else(|| "native receipt consumed".to_owned())?;
                self.consume_count_receipt(&result.plan, actual)
                    .map_err(|_| "native final admission unavailable".to_owned())
            }),
        )
        .await
        .map_err(|_| PilotAuthorityError::Expired)?
        .map_err(|_| PilotAuthorityError::Unavailable)?;
        // Any HTTP rejection is charged and terminal. No 401 recovery or retry.
        if !response.status.is_success() {
            return Err(PilotAuthorityError::Denied);
        }
        guard.successful = true;
        // Body/decoder custody remains live even after returning response headers.
        let bytes = response.bytes;
        response.bytes = Box::pin(futures::stream::unfold(
            (bytes, lease),
            |(mut bytes, lease)| async move { bytes.next().await.map(|chunk| (chunk, (bytes, lease))) },
        ));
        Ok((response, attempt))
    }

    fn recheck_count_plan(
        &self,
        plan: &CountPlan,
        request: &Request,
        boundary: CountBoundary,
    ) -> Result<(), PilotAuthorityError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| PilotAuthorityError::Unavailable)?;
        if state.revoked || self.events.is_closed() {
            return Err(PilotAuthorityError::Expired);
        }
        let audit = state
            .active_capture
            .as_ref()
            .ok_or(PilotAuthorityError::Denied)?;
        if !audit.matches_pending_count(plan.decision_id, plan.attempt_id, plan.request_id) {
            return Err(PilotAuthorityError::Denied);
        }
        let ledger = state
            .pilot
            .as_mut()
            .ok_or(PilotAuthorityError::Unavailable)?;
        ledger.validate_count_plan(
            (self.clock)().map_err(|_| PilotAuthorityError::Unavailable)?,
            plan,
        )?;
        match boundary {
            CountBoundary::Count => {
                let mut authenticated = request.clone();
                ledger
                    .issuer
                    .authenticate_count_request(&ledger.claims, &mut authenticated)?;
                if authenticated.headers != request.headers
                    || request.url != plan.scope.count_url
                    || !matches!(&request.body, Some(RequestBody::EncodedJson(body)) if body.as_bytes() == plan.wire.count_body().as_bytes())
                {
                    return Err(PilotAuthorityError::Denied);
                }
            }
            CountBoundary::Inference => {
                ledger
                    .issuer
                    .validate_count_inference(&ledger.claims, request)?;
                if request.url != plan.scope.inference_url
                    || !matches!(&request.body, Some(RequestBody::EncodedJson(body)) if body.as_bytes() == plan.wire.inference_body().as_bytes())
                {
                    return Err(PilotAuthorityError::Denied);
                }
            }
        }
        if request.method != http::Method::POST || request.compression != RequestCompression::None {
            return Err(PilotAuthorityError::Denied);
        }
        Ok(())
    }
}

async fn send_once(
    factory: &codex_http_client::HttpClientFactory,
    url: &str,
    request: Request,
    gate: impl FnOnce(&Request) -> Result<(), String> + Send,
) -> Result<StreamResponse, codex_http_client::TransportError> {
    #[cfg(all(test, target_os = "linux"))]
    if let Some(controlled) = request
        .extensions
        .get::<Arc<tests::ControlledIo>>()
        .cloned()
    {
        return controlled.send(request, gate).await;
    }
    SingleAttemptTransport::new(factory, url)?
        .stream(request, gate)
        .await
}

#[cfg(all(test, target_os = "linux"))]
pub(super) use tests::ControlledIo;

#[cfg(all(test, target_os = "linux"))]
#[path = "observation_pilot_count_operation_tests.rs"]
mod tests;

enum CountBoundary {
    Count,
    Inference,
}
