use super::*;
use pretty_assertions::assert_eq;

#[test]
fn preserves_original_binary64_identity_without_decimal_rounding() {
    for bits in [
        1_u64,
        0x3ff0_0000_0000_0000,
        0x3ff8_0000_0000_0000,
        0x4278_bcfe_5680_0801,
        0x7fef_ffff_ffff_ffff,
    ] {
        assert_eq!(
            ObservationHostPreparation::from_wire(
                /*cooldown_revision*/ 1,
                &format!("{bits:016x}")
            ),
            Ok(ObservationHostPreparation {
                cooldown_revision: 1,
                wake_not_before_bits: bits,
            })
        );
    }
}

#[test]
fn refuses_invalid_preparation_before_any_admission() {
    for bits in [
        "0000000000000000",
        "8000000000000000",
        "bff0000000000000",
        "7ff0000000000000",
        "fff0000000000000",
        "7ff8000000000001",
        "3FF0000000000000",
        "3ff000000000000",
        "03ff0000000000000",
        "+ff0000000000000",
    ] {
        assert_eq!(
            ObservationHostPreparation::from_wire(/*cooldown_revision*/ 1, bits),
            Err(ObservationError::InvalidFrame)
        );
    }
    for revision in [0, 1_u64 << 53, u64::MAX] {
        assert_eq!(
            ObservationHostPreparation::from_wire(revision, "3ff8000000000000"),
            Err(ObservationError::InvalidFrame)
        );
    }
}
