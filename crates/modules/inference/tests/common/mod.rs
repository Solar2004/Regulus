//! A fake model server for tests: answers each HTTP request with what `answer` returns
//! for its path and JSON body, and keeps every request. One request per connection.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use serde_json::Value;

pub type Seen = Arc<Mutex<Vec<(String, Value, Vec<String>)>>>;

pub struct Fake {
    pub url: String,
    pub seen: Seen,
}

pub fn serve(answer: impl Fn(usize, &str, &Value) -> (u16, Value) + Send + 'static) -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let seen: Seen = Arc::default();
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
            let (mut length, mut headers) = (0, Vec::new());
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                let header = header.trim_end().to_string();
                if header.is_empty() {
                    break;
                }
                if let Some((name, value)) = header.split_once(':') {
                    if name.eq_ignore_ascii_case("content-length") {
                        length = value.trim().parse().unwrap();
                    }
                }
                headers.push(header.to_ascii_lowercase());
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let count = {
                let mut log = log.lock().unwrap();
                log.push((path.clone(), body.clone(), headers));
                log.len()
            };
            let (status, reply) = answer(count, &path, &body);
            let reply = reply.to_string();
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
                 connection: close\r\n\r\n{reply}",
                reply.len()
            );
        }
    });
    Fake { url, seen }
}

/// An OpenAI chat completion whose message content is `content`.
pub fn completion(content: &str) -> Value {
    serde_json::json!({ "choices": [{ "message": { "role": "assistant", "content": content } }] })
}
