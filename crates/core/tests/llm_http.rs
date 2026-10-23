//! The HTTP client against a local mock server: retries, errors, truncation,
//! and OpenAI-compatible parameter fallbacks.
#![allow(clippy::type_complexity)]

use serde_json::{json, Value};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use yakusu_core::llm::{self, Client, Llm, LlmError};

type Reply = (u16, Value, Vec<(&'static str, &'static str)>);

struct Mock {
    url: String,
    queue: Arc<Mutex<Vec<Reply>>>,
    requests: Arc<Mutex<Vec<(Vec<(String, String)>, Value)>>>,
    slept: Arc<Mutex<Vec<f64>>>,
}

fn mock() -> Mock {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}", server.server_addr().to_ip().unwrap());
    let queue: Arc<Mutex<Vec<Reply>>> = Default::default();
    let requests: Arc<Mutex<Vec<(Vec<(String, String)>, Value)>>> = Default::default();
    let (q, r) = (queue.clone(), requests.clone());
    std::thread::spawn(move || {
        for mut req in server.incoming_requests() {
            let mut body = String::new();
            req.as_reader().read_to_string(&mut body).ok();
            let headers =
                req.headers().iter().map(|h| (h.field.to_string().to_lowercase(), h.value.to_string())).collect();
            r.lock().unwrap().push((headers, serde_json::from_str(&body).unwrap_or(Value::Null)));
            let (status, body, hdrs) = {
                let mut q = q.lock().unwrap();
                if q.is_empty() {
                    (500, json!({"error": {"message": "empty"}}), vec![])
                } else {
                    q.remove(0)
                }
            };
            let mut resp = tiny_http::Response::from_string(body.to_string()).with_status_code(status);
            for (k, v) in hdrs {
                resp.add_header(tiny_http::Header::from_bytes(k.as_bytes(), v.as_bytes()).unwrap());
            }
            req.respond(resp).ok();
        }
    });
    Mock { url, queue, requests, slept: Default::default() }
}

fn anthropic_ok(text: &str, stop: &str) -> Reply {
    (
        200,
        json!({"type": "message", "content": [{"type": "text", "text": text}], "stop_reason": stop,
                 "usage": {"input_tokens": 100, "output_tokens": 20, "cache_read_input_tokens": 50,
                           "cache_creation_input_tokens": 0}}),
        vec![],
    )
}

fn client(m: &Mock, provider: &str, model: &str) -> Client {
    let url = format!("{}{}", m.url, if provider == "anthropic" { "/v1/messages" } else { "/v1" });
    let mut c = Client::new(provider, "key-123", model, Some(&url));
    let slept = m.slept.clone();
    c.sleep = Box::new(move |s, _| slept.lock().unwrap().push(s));
    c
}

fn go(c: &Client) -> Result<llm::Completion, LlmError> {
    c.complete("SYS", "USER", 500, &AtomicBool::new(false))
}

fn header<'a>(h: &'a [(String, String)], k: &str) -> Option<&'a str> {
    h.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
}

#[test]
fn anthropic_request_shape_and_cost() {
    let m = mock();
    m.queue.lock().unwrap().push(anthropic_ok("[{\"i\": 1, \"en\": \"Hi\"}]", "end_turn"));
    let c = go(&client(&m, "anthropic", "claude-haiku-5-5")).unwrap();
    let reqs = m.requests.lock().unwrap();
    let (h, body) = &reqs[0];
    assert_eq!(header(h, "x-api-key"), Some("key-123"));
    assert_eq!(header(h, "anthropic-version"), Some(llm::ANTHROPIC_VERSION));
    assert_eq!(body["system"][0]["cache_control"], json!({"type": "ephemeral"}));
    assert_eq!(body["messages"], json!([{"role": "user", "content": "USER"}]));
    assert_eq!(c.usage, llm::Usage { input: 100, output: 20, cache_write: 0, cache_read: 50 });
    assert!((c.usd - (100.0 * 0.10 + 20.0 * 0.50 + 50.0 * 0.01) / 1e6).abs() < 1e-12);
}

#[test]
fn rate_limit_waits_for_retry_after_then_succeeds() {
    let m = mock();
    m.queue.lock().unwrap().extend([
        (429, json!({"error": {"message": "slow down"}}), vec![("retry-after", "7")]),
        (529, json!({"error": {"message": "overloaded"}}), vec![]),
        anthropic_ok("ok", "end_turn"),
    ]);
    let c = go(&client(&m, "anthropic", "claude-haiku-5-5")).unwrap();
    assert_eq!(c.text, "ok");
    assert_eq!(m.slept.lock().unwrap()[0], 7.0);
}

#[test]
fn bad_key_is_fatal_immediately() {
    let m = mock();
    m.queue
        .lock()
        .unwrap()
        .extend([(401, json!({"error": {"message": "invalid x-api-key"}}), vec![]), anthropic_ok("never", "end_turn")]);
    match go(&client(&m, "anthropic", "claude-haiku-5-5")) {
        Err(LlmError::Fatal(msg)) => assert!(msg.contains("rejected")),
        _ => panic!("expected fatal"),
    }
    assert_eq!(m.requests.lock().unwrap().len(), 1);
}

#[test]
fn out_of_credit_is_explained() {
    let m = mock();
    m.queue.lock().unwrap().push((
        400,
        json!({"error": {"message": "Your credit balance is too low to access the API."}}),
        vec![],
    ));
    match go(&client(&m, "anthropic", "claude-haiku-5-5")) {
        Err(LlmError::Fatal(msg)) => assert!(msg.contains("out of credit")),
        _ => panic!("expected fatal"),
    }
}

#[test]
fn truncated_reply_is_reported_with_usage() {
    let m = mock();
    m.queue.lock().unwrap().push(anthropic_ok("[{\"i\": 1, \"en\": \"Hal", "max_tokens"));
    match go(&client(&m, "anthropic", "claude-haiku-5-5")) {
        Err(LlmError::Truncated(u)) => assert_eq!(u.output, 20),
        _ => panic!("expected truncated"),
    }
    assert_eq!(m.requests.lock().unwrap().len(), 1); // not retried blindly
}

#[test]
fn gives_up_after_repeated_server_errors() {
    let m = mock();
    m.queue.lock().unwrap().extend(std::iter::repeat_n((500, json!({"error": {"message": "boom"}}), vec![]), 6));
    match go(&client(&m, "anthropic", "claude-haiku-5-5")) {
        Err(LlmError::Fatal(msg)) => assert!(msg.contains("Gave up")),
        _ => panic!("expected fatal"),
    }
}

#[test]
fn openai_adapts_parameters() {
    let m = mock();
    let ok = (
        200,
        json!({"choices": [{"message": {"content": "hello"}, "finish_reason": "stop"}],
                          "usage": {"prompt_tokens": 10, "completion_tokens": 2}}),
        vec![],
    );
    // OpenAI proper: starts with max_completion_tokens and no temperature.
    m.queue.lock().unwrap().push(ok.clone());
    assert_eq!(go(&client(&m, "openai", "some-model")).unwrap().text, "hello");
    {
        let reqs = m.requests.lock().unwrap();
        let (h, body) = reqs.last().unwrap();
        assert!(body.get("max_completion_tokens").is_some() && body.get("temperature").is_none());
        assert_eq!(header(h, "authorization"), Some("Bearer key-123"));
    }
    // A server that rejects temperature gets a retry without it.
    m.queue
        .lock()
        .unwrap()
        .extend([(400, json!({"error": {"message": "Unsupported parameter: 'temperature'"}}), vec![]), ok]);
    assert_eq!(go(&client(&m, "openrouter", "m")).unwrap().text, "hello");
    let reqs = m.requests.lock().unwrap();
    let n = reqs.len();
    assert!(reqs[n - 2].1.get("temperature").is_some() && reqs[n - 1].1.get("temperature").is_none());
}

#[test]
fn openai_length_finish_is_truncation() {
    let m = mock();
    m.queue.lock().unwrap().push((
        200,
        json!({"choices": [{"message": {"content": "[{"}, "finish_reason": "length"}], "usage": {}}),
        vec![],
    ));
    assert!(matches!(go(&client(&m, "local", "m")), Err(LlmError::Truncated(_))));
}

#[test]
fn local_server_down_is_explained() {
    let c = Client::new("local", "", "m", Some("http://127.0.0.1:9/v1"));
    match c.complete("s", "u", 10, &AtomicBool::new(false)) {
        Err(LlmError::Fatal(msg)) => assert!(msg.contains("LM Studio"), "{msg}"),
        _ => panic!("expected fatal"),
    }
}

#[test]
fn parse_json_array_tolerates_fences_and_thinking() {
    assert_eq!(llm::parse_json_array("```json\n[{\"i\":1}]\n```").unwrap(), vec![json!({"i": 1})]);
    assert_eq!(llm::parse_json_array("<think>[nope]</think>Sure: [{\"i\":2}] done").unwrap(), vec![json!({"i": 2})]);
    assert!(llm::parse_json_array("no array here").is_err());
}
