use crate::ObservationSlot;
use crate::observation::ObservationAttempt;
use crate::observation::ObservationTransportLease;
use codex_api::RequestTelemetry;
use codex_api::SseTelemetry;
use codex_client::TransportError;
use http::HeaderMap;
use http::StatusCode;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use uuid::Uuid;

struct ObservationTelemetry {
    lease: Arc<ObservationTransportLease>,
    decision_id: Uuid,
    attempt: Mutex<Option<ObservationAttempt>>,
    request: Arc<dyn RequestTelemetry>,
    sse: Arc<dyn SseTelemetry>,
}

pub(super) fn wrap(
    slot: Arc<ObservationSlot>,
    decision_id: Uuid,
    request: Arc<dyn RequestTelemetry>,
    sse: Arc<dyn SseTelemetry>,
) -> (Arc<dyn RequestTelemetry>, Arc<dyn SseTelemetry>) {
    let telemetry = Arc::new(ObservationTelemetry {
        lease: slot.transport_lease(),
        decision_id,
        attempt: Mutex::new(None),
        request,
        sse,
    });
    (telemetry.clone(), telemetry)
}

impl ObservationTelemetry {
    fn start_request(&self, request: Option<&codex_client::Request>) -> Result<(), String> {
        // The concrete hook never waits for capacity or a contended mutex and
        // never returns provider/body text. Both error strings are fixed/bounded.
        let mut attempt = self
            .attempt
            .try_lock()
            .map_err(|_| "observation audit admission unavailable".to_owned())?;
        let upstream = match request {
            Some(request) => self.request.on_request_prepared(request),
            None => self.request.on_request_start(),
        };
        upstream.map_err(|_| "observation audit admission unavailable".to_owned())?;
        let started = match request {
            Some(request) => self
                .lease
                .slot()
                .begin_attempt_for_request(self.decision_id, request),
            None => self.lease.slot().begin_attempt(self.decision_id),
        };
        *attempt = Some(started.map_err(|_| "observation audit admission unavailable".to_owned())?);
        Ok(())
    }
}

impl RequestTelemetry for ObservationTelemetry {
    fn authenticate_request(
        &self,
        request: &mut codex_client::Request,
    ) -> Result<codex_client::RequestAuthentication, String> {
        match self
            .lease
            .slot()
            .authenticate_pilot_request(request)
            .map_err(|_| "observation credential unavailable".to_owned())?
        {
            codex_client::RequestAuthentication::Prepared => {
                Ok(codex_client::RequestAuthentication::Prepared)
            }
            codex_client::RequestAuthentication::Provider => {
                self.request.authenticate_request(request)
            }
        }
    }

    fn on_request_start(&self) -> Result<(), String> {
        self.start_request(/*request*/ None)
    }

    fn on_request_prepared(&self, request: &codex_client::Request) -> Result<(), String> {
        self.start_request(Some(request))
    }

    fn on_response_headers(&self, headers: &HeaderMap) {
        self.request.on_response_headers(headers);
        if let Ok(attempt) = self.attempt.lock()
            && let Some(attempt) = *attempt
            && let Err(error) = self.lease.slot().attempt_headers(
                attempt,
                headers.get("x-request-id").and_then(|id| id.to_str().ok()),
            )
        {
            tracing::warn!(%error, "observation audit header unavailable");
        }
    }

    fn on_request(
        &self,
        attempt: u64,
        status: Option<StatusCode>,
        error: Option<&TransportError>,
        duration: Duration,
    ) {
        // Includes auth/admission failures. It neither allocates a wire ID nor
        // overwrites an accepted attempt with a later error or a failed retry.
        self.request.on_request(attempt, status, error, duration);
    }
}

impl SseTelemetry for ObservationTelemetry {
    fn response_created_callback(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        let attempt = (*self.attempt.lock().ok()?)?;
        let lease = Arc::clone(&self.lease);
        let upstream = self.sse.response_created_callback();
        // Copy the concrete attempt, not the mutable latest-attempt slot. The
        // decoder calls this before forwarding created even if the consumer drops.
        Some(Arc::new(move || {
            if let Err(error) = lease.slot().attempt_accepted(attempt) {
                tracing::warn!(%error, "observation audit acceptance unavailable");
            }
            if let Some(callback) = &upstream {
                callback();
            }
        }))
    }

    fn response_completed_callback(&self) -> Option<codex_api::ResponseCompletedCallback> {
        let attempt = (*self.attempt.lock().ok()?)?;
        let lease = Arc::clone(&self.lease);
        let upstream = self.sse.response_completed_callback();
        Some(Arc::new(move |response_id, usage| {
            if let Err(error) = lease
                .slot()
                .pilot_attempt_completed(attempt, response_id, usage)
            {
                tracing::warn!(%error, "pilot usage reconciliation unavailable");
            }
            if let Some(callback) = &upstream {
                callback(response_id, usage);
            }
        }))
    }

    fn on_sse_poll(
        &self,
        result: &Result<
            Option<
                Result<
                    eventsource_stream::Event,
                    eventsource_stream::EventStreamError<TransportError>,
                >,
            >,
            tokio::time::error::Elapsed,
        >,
        duration: Duration,
    ) {
        self.sse.on_sse_poll(result, duration);
    }
}
