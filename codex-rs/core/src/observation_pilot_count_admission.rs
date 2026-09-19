//! Original-ledger post-await checks and final single-use reservation consumption.
use super::*;

impl PilotLedger {
    pub(in crate::observation) fn consume_count_plan(
        &mut self,
        now: ObservationClock,
        plan: &CountPlan,
        actual: &Request,
    ) -> Result<PilotRequestReservation, PilotAuthorityError> {
        self.validate_count_plan(now, plan)?;
        self.issuer.validate_count_inference(&self.claims, actual)?;
        if actual.url != plan.scope.inference_url
            || actual.method != http::Method::POST
            || actual.compression != RequestCompression::None
            || actual.headers.contains_key(http::header::CONTENT_ENCODING)
            || !matches!(&actual.body, Some(RequestBody::EncodedJson(body))
                if body.as_bytes() == plan.wire.inference_body().as_bytes())
        {
            return Err(PilotAuthorityError::Denied);
        }
        let pending = self
            .pending_counts
            .get_mut(plan.operation)
            .ok_or(PilotAuthorityError::Denied)?;
        if !pending.completed {
            return Err(PilotAuthorityError::Unavailable);
        }
        pending.consumed = true;
        Ok(plan.reservation.clone())
    }

    pub(in crate::observation) fn validate_count_plan(
        &mut self,
        now: ObservationClock,
        plan: &CountPlan,
    ) -> Result<(), PilotAuthorityError> {
        self.expired |= now.monotonic >= plan.deadline
            || now.wall_seconds >= plan.scope.expires_at
            || self.issuer.recheck_grant(&self.claims).is_err()
            || plan.journal.failed();
        if self.expired {
            return Err(PilotAuthorityError::Expired);
        }
        if self.issuer.count_scope(&self.claims)?.as_ref() != Some(&plan.scope) {
            self.expired = true;
            return Err(PilotAuthorityError::Denied);
        }
        let pending = self
            .pending_counts
            .get(plan.operation)
            .ok_or(PilotAuthorityError::Denied)?;
        if !Arc::ptr_eq(&pending.brand, &plan.brand)
            || !pending.durable
            || pending.consumed
            || !Arc::ptr_eq(&pending.debit, &plan.debit)
            || pending.reservation != plan.reservation
        {
            return Err(PilotAuthorityError::Replay);
        }
        Ok(())
    }
}
