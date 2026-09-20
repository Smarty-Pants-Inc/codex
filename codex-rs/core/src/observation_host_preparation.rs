//! Exact host cooldown provenance. This value is not admission authority.
use crate::ObservationError;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservationHostPreparation {
    pub(crate) cooldown_revision: u64,
    pub(crate) wake_not_before_bits: u64,
}

impl ObservationHostPreparation {
    pub fn from_wire(
        cooldown_revision: u64,
        wake_not_before_bits: &str,
    ) -> Result<Self, ObservationError> {
        if cooldown_revision == 0
            || cooldown_revision > (1_u64 << 53) - 1
            || wake_not_before_bits.len() != 16
            || !wake_not_before_bits
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ObservationError::InvalidFrame);
        }
        let bits = u64::from_str_radix(wake_not_before_bits, 16)
            .map_err(|_| ObservationError::InvalidFrame)?;
        let timestamp = f64::from_bits(bits);
        if !timestamp.is_finite() || timestamp <= 0.0 {
            return Err(ObservationError::InvalidFrame);
        }
        Ok(Self {
            cooldown_revision,
            wake_not_before_bits: bits,
        })
    }
}

#[cfg(test)]
#[path = "observation_host_preparation_tests.rs"]
mod tests;
