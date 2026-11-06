//! Loads a saved article from a local HTTP server through the real Servo helper binary.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::thread;

const ARTICLE: &str = include_str!("fixtures/article.html");

/// Serves the fixture at `/article` (with a `probe` cookie) and 404 for everything else.
fn serve(listener: TcpListener) {
    for stream in listener.incoming().flatten() {
        thread::spawn(move || answer(stream));
    }
}

fn answer(mut stream: TcpStream) {
    let mut request_line = String::new();
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    // Drain the headers.
    let mut line = String::new();
    while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
        line.clear();
    }
    let path = request_line.split_whitespace().nth(1).unwrap_or("");
    let response = if path == "/article" {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
             Set-Cookie: probe=1; Path=/\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            ARTICLE.len(),
            ARTICLE
        )
    } else {
        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
    };
    let _ = stream.write_all(response.as_bytes());
}

#[test]
fn extracts_text_from_local_article() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    thread::spawn(move || serve(listener));

    let output = Command::new(env!("CARGO_BIN_EXE_juttusihti-browser"))
        .arg("--probe")
        .arg(format!("http://127.0.0.1:{port}/article"))
        .output()
        .expect("run helper");
    assert!(output.status.success(), "helper failed: {}", String::from_utf8_lossy(&output.stderr));
    let extracted: serde_json::Value = serde_json::from_slice(&output.stdout).expect("helper prints JSON");

    let text = extracted["text"].as_str().expect("text");
    assert!(text.contains("Councillors argued for three hours about the cost of the bridge"), "text: {text}");
    let picture = extracted["picture_url"].as_str().expect("picture_url");
    assert!(picture.ends_with("/hero.jpg"), "picture_url: {picture}");
    assert_eq!(extracted["paywalled"], true);
    assert_eq!(extracted["cookie_names"], serde_json::json!(["probe"]));
}
