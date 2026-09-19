//! Exact vectors from V2-CANONICAL-BYTE-FIXTURES.json, shared with Foundation.
use super::*;
use pretty_assertions::assert_eq;
use sha2::Digest;
use sha2::Sha256;

#[test]
fn accepted_helper_python_bytes_match_shared_vectors() -> anyhow::Result<()> {
    for (input, expected_hex, expected_sha256) in [
        (
            r###"{"z":0,"a":{"z":2,"a":1}}"###,
            "7b2261223a7b2261223a312c227a223a327d2c227a223a307d0a",
            "cb2f39d76b226e0432f345320ffe17beea55e83fbacd4dd00c07ac52a15b1e8f",
        ),
        (
            r###"{"𐀀":"astral","":"bmp","a":"ascii"}"###,
            "7b2261223a226173636969222c22ee8080223a22626d70222c22f0908080223a2261737472616c227d0a",
            "a6da00f33080e9bdbd8eaf37f3ab8973598516aba49d41b7601e6d04afde5618",
        ),
        (
            r###"{"value":"é é 😀 / \\ \" \b\f\n\r\t\u0000\u001f  "}"###,
            "7b2276616c7565223a22c3a92065cc8120f09f9880202f205c5c205c22205c625c665c6e5c725c745c75303030305c7530303166e280a8e280a9227d0a",
            "70c8a425bf35b81f573ab036f501d5ccae885803226f4748032ace49c88d7688",
        ),
        (
            r###"{"zero":0,"max":9007199254740991,"maxMinusOne":9007199254740990}"###,
            "7b226d6178223a393030373139393235343734303939312c226d61784d696e75734f6e65223a393030373139393235343734303939302c227a65726f223a307d0a",
            "250d1d19120028a3a957337aa09012b76b17d96227820c8350b7ebccefdf509a",
        ),
    ] {
        let value = serde_json::from_str(input)?;
        let bytes = encode(&value)?;
        let actual_hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(
            (actual_hex, format!("{:x}", Sha256::digest(&bytes))),
            (expected_hex.to_owned(), expected_sha256.to_owned())
        );
        validate(&bytes)?;
    }
    Ok(())
}

#[test]
fn alternate_encodings_do_not_become_canonical_with_a_matching_digest() {
    for bytes in [
        &b"{\"z\":0,\"a\":1}\n"[..],
        &b"{\"a\":{\"z\":0,\"a\":1}}\n"[..],
        &b"{\"a\":1,\"a\":1}\n"[..],
        &b"{\"a\":1.0}\n"[..],
        &b"{\"a\":1e0}\n"[..],
        &b"{\"a\":-0}\n"[..],
        &b"{\"a\":9007199254740992}\n"[..],
        &b"{\"a\":1}\n\n"[..],
        &b"{\"a\":1}"[..],
        &b"{\"a\":\"\\u00e9\"}\n"[..],
    ] {
        assert!(validate(bytes).is_err());
    }
}
