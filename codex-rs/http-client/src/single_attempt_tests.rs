use super::SingleAttemptTransport;
use crate::EncodedJsonBody;
use crate::HttpClientFactory;
use crate::OutboundProxyPolicy;
use crate::Request;
use crate::RequestBody;
use crate::RequestCompression;
use futures::TryStreamExt;
use http::Method;
use pretty_assertions::assert_eq;
use std::io::Read;
use std::io::Write;
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

// Valid gzip encoding of an empty body. The caller must receive these bytes,
// not the decoded empty payload.
const GZIP: &[u8] = &[
    31, 139, 8, 0, 0, 0, 0, 0, 0, 3, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

#[test]
fn rejects_invalid_destinations() {
    let factory = HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault);
    for url in [
        "not a URL",
        "http://localhost/",
        "https://LOCALHOST/",
        "https://localhost",
        "https://user@localhost/",
        "https://user:pass@localhost/",
        "https://localhost/#fragment",
    ] {
        assert!(SingleAttemptTransport::new(&factory, url).is_err(), "{url}");
    }
}

#[test]
fn https_single_attempt_contract() {
    codex_utils_rustls_provider::ensure_rustls_crypto_provider();
    let certificate =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("certificate");
    let directory = tempfile::tempdir().expect("certificate directory");
    let ca = directory.path().join("ca.pem");
    std::fs::write(&ca, certificate.cert.pem()).expect("write CA");
    let config = Arc::new(
        rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![certificate.cert.der().clone()],
                rustls::pki_types::PrivatePkcs8KeyDer::from(
                    certificate.signing_key.serialize_der(),
                )
                .into(),
            )
            .expect("TLS configuration"),
    );

    for scenario in [
        "ok", "encoded", "gate", "invalid", "307", "308", "503", "drop", "gzip",
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        listener.set_nonblocking(true).expect("nonblocking");
        let url = format!(
            "https://localhost:{}/",
            listener.local_addr().expect("address").port()
        );
        let stop = Arc::new(AtomicBool::new(false));
        let server_stop = Arc::clone(&stop);
        let config = Arc::clone(&config);
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            while !server_stop.load(Ordering::SeqCst) {
                let (socket, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("accept: {error}"),
                };
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .expect("read deadline");
                socket
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .expect("write deadline");
                let connection = rustls::ServerConnection::new(Arc::clone(&config)).expect("TLS");
                let mut stream = rustls::StreamOwned::new(connection, socket);
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).expect("request header");
                    request.push(byte[0]);
                    assert!(request.len() < 16384);
                }
                let header = String::from_utf8(request).expect("HTTP header");
                let length: usize = header
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse().expect("length"))
                    })
                    .expect("body length");
                assert!(length < 16384);
                let mut body = vec![0; length];
                stream.read_exact(&mut body).expect("request body");
                requests.push((
                    header.lines().next().expect("request line").to_owned(),
                    body,
                ));
                if scenario == "drop" {
                    continue;
                }
                let status = match scenario {
                    "307" => 307,
                    "308" => 308,
                    "503" => 503,
                    _ => 200,
                };
                let body = if scenario == "gzip" {
                    GZIP
                } else {
                    b"response"
                };
                let encoding = if scenario == "gzip" {
                    "Content-Encoding: gzip\r\n"
                } else {
                    ""
                };
                let response = format!(
                    "HTTP/1.1 {status} Fixture\r\nLocation: /redirect-target\r\n{encoding}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("response header");
                stream.write_all(body).expect("response body");
                stream.flush().expect("flush");
            }
            requests
        });
        // Subprocess-scoped trust, as in the existing custom-CA fixtures: never
        // mutate the test process environment or weaken HTTPS verification.
        let result = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "single_attempt::tests::https_contract_child",
                "--nocapture",
            ])
            .env("CODEX_SINGLE_ATTEMPT_FIXTURE", scenario)
            .env("CODEX_SINGLE_ATTEMPT_URL", &url)
            .env("SSL_CERT_FILE", &ca)
            .env_remove("CODEX_CA_CERTIFICATE")
            .env_remove("HTTPS_PROXY")
            .env_remove("https_proxy")
            .env_remove("HTTP_PROXY")
            .env_remove("http_proxy")
            .env_remove("ALL_PROXY")
            .env_remove("all_proxy")
            .env("NO_PROXY", "localhost,127.0.0.1")
            .output()
            .expect("child test");
        stop.store(true, Ordering::SeqCst);
        let requests = server.join().expect("fixture thread");
        assert!(
            result.status.success(),
            "{scenario}: {} {}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        let expected = if matches!(scenario, "gate" | "invalid") {
            vec![]
        } else {
            vec![(
                "POST / HTTP/1.1".to_string(),
                serde_json::to_vec(&serde_json::json!({"text": "é\"\\"})).expect("expected JSON"),
            )]
        };
        assert_eq!(requests, expected, "{scenario}");
    }
}

#[tokio::test]
async fn https_contract_child() {
    let Ok(scenario) = std::env::var("CODEX_SINGLE_ATTEMPT_FIXTURE") else {
        return;
    };
    let url = std::env::var("CODEX_SINGLE_ATTEMPT_URL").expect("fixture URL");
    let factory = HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault);
    let value = serde_json::json!({"text": "é\"\\"});
    let mut request = Request::new(Method::POST, url.clone());
    request.timeout = Some(Duration::from_secs(5));
    request.body = Some(RequestBody::Json(value.clone()));
    if scenario == "invalid" {
        for mutation in [
            "url",
            "method",
            "compression",
            "content-encoding",
            "host",
            "content-length",
            "transfer-encoding",
            "content-type",
        ] {
            let mut invalid = request.clone();
            match mutation {
                "url" => invalid.url.push_str("different"),
                "method" => invalid.method = Method::GET,
                "compression" => invalid.compression = RequestCompression::Zstd,
                header => {
                    invalid.headers.insert(
                        http::header::HeaderName::from_bytes(header.as_bytes()).expect("header"),
                        http::HeaderValue::from_static("invalid"),
                    );
                }
            }
            let result = SingleAttemptTransport::new(&factory, &url)
                .expect("transport")
                .stream(invalid, |_| panic!("invalid request reached gate"))
                .await;
            assert!(result.is_err(), "{mutation}");
        }
        return;
    }
    if scenario == "encoded" {
        request.body = Some(RequestBody::EncodedJson(
            EncodedJsonBody::encode(&value)
                .expect("encode")
                .without_body_trace(),
        ));
        request = request.into_prepared().expect("preparation");
    }
    let mut gates = 0;
    let result = SingleAttemptTransport::new(&factory, &url)
        .expect("transport")
        .stream(request, |prepared| {
            gates += 1;
            let body = prepared.prepare_body_for_send().expect("prepared body");
            assert_eq!(
                body.body.as_deref(),
                Some(serde_json::to_vec(&value).expect("JSON").as_slice())
            );
            assert_eq!(
                prepared.headers.get(http::header::CONTENT_TYPE),
                Some(&http::HeaderValue::from_static("application/json"))
            );
            if scenario == "gate" {
                Err("gate refused".into())
            } else {
                Ok(())
            }
        })
        .await;
    assert_eq!(gates, 1);
    if matches!(scenario.as_str(), "gate" | "drop") {
        assert!(result.is_err());
        return;
    }
    let response = result.expect("response");
    let status = response.status.as_u16();
    let encoding = response
        .headers
        .get(http::header::CONTENT_ENCODING)
        .cloned();
    let chunks: Vec<_> = response.bytes.try_collect().await.expect("response bytes");
    let bytes: Vec<u8> = chunks.into_iter().flatten().collect();
    let expected_status = match scenario.as_str() {
        "307" => 307,
        "308" => 308,
        "503" => 503,
        _ => 200,
    };
    let expected_encoding = (scenario == "gzip").then(|| http::HeaderValue::from_static("gzip"));
    let expected_body = if scenario == "gzip" {
        GZIP
    } else {
        b"response"
    };
    assert_eq!(
        (status, encoding, bytes),
        (expected_status, expected_encoding, expected_body.to_vec())
    );
}
