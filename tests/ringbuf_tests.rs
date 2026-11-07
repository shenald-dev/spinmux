use chrono::Utc;
use spinmux::ringbuf::{
    spawn_stream_reader, strip_ansi, LogBuffer, LogEntry, LogStream, RingBuffer,
};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;

#[tokio::test]
async fn test_basic_push_and_snapshot() {
    let buffer = RingBuffer::new(100, 1024 * 1024);
    assert!(buffer.is_empty());
    assert_eq!(buffer.len(), 0);

    buffer.push(LogEntry::new(
        "api",
        LogStream::Stdout,
        "Server listening on 8080",
    ));
    buffer.push(LogEntry::new(
        "api",
        LogStream::Stderr,
        "Connection warning",
    ));

    assert_eq!(buffer.len(), 2);
    assert!(!buffer.is_empty());

    let snapshot = buffer.snapshot(10);
    assert_eq!(snapshot.len(), 2);
    assert_eq!(snapshot[0].message, "Server listening on 8080");
    assert_eq!(snapshot[0].stream, LogStream::Stdout);
    assert_eq!(snapshot[0].sequence, 0);

    assert_eq!(snapshot[1].message, "Connection warning");
    assert_eq!(snapshot[1].stream, LogStream::Stderr);
    assert_eq!(snapshot[1].sequence, 1);
}

#[test]
fn test_log_buffer_trait_dispatch() {
    let buffer: Box<dyn LogBuffer> = Box::new(RingBuffer::new(10, 1024));
    buffer.push(LogEntry::new("test", LogStream::System, "Trait message"));
    assert_eq!(buffer.snapshot(5).len(), 1);
    buffer.clear();
    assert_eq!(buffer.snapshot(5).len(), 0);
}

#[tokio::test]
async fn test_snapshot_limit_bounds() {
    let buffer = RingBuffer::new(50, 1024 * 1024);

    for i in 0..10 {
        buffer.push(LogEntry::new(
            "web",
            LogStream::Stdout,
            format!("Line {}", i),
        ));
    }

    assert_eq!(buffer.len(), 10);

    // Limit 0 returns empty
    assert!(buffer.snapshot(0).is_empty());

    // Limit 3 returns the 3 most recent entries in chronological order
    let snap3 = buffer.snapshot(3);
    assert_eq!(snap3.len(), 3);
    assert_eq!(snap3[0].message, "Line 7");
    assert_eq!(snap3[1].message, "Line 8");
    assert_eq!(snap3[2].message, "Line 9");

    // Limit 10 returns all 10 entries
    let snap10 = buffer.snapshot(10);
    assert_eq!(snap10.len(), 10);
    assert_eq!(snap10[0].message, "Line 0");
    assert_eq!(snap10[9].message, "Line 9");

    // Limit exceeding count clamps to available count
    let snap50 = buffer.snapshot(50);
    assert_eq!(snap50.len(), 10);
}

#[tokio::test]
async fn test_circular_wraparound_drops_oldest() {
    let capacity = 5;
    let buffer = RingBuffer::new(capacity, 1024 * 1024);

    // Push 12 entries into a buffer with capacity 5
    for i in 0..12 {
        buffer.push(LogEntry::new(
            "worker",
            LogStream::Stdout,
            format!("Item {}", i),
        ));
    }

    // Retained count must equal capacity
    assert_eq!(buffer.len(), capacity);
    assert_eq!(buffer.dropped_count(), 7);

    // Snapshot of capacity must contain the 5 newest items: 7, 8, 9, 10, 11
    let snap = buffer.snapshot(capacity);
    assert_eq!(snap.len(), 5);
    for (idx, expected) in (7..12).enumerate() {
        assert_eq!(snap[idx].message, format!("Item {}", expected));
        assert_eq!(snap[idx].sequence, expected as u64);
    }
}

#[tokio::test]
async fn test_high_throughput_concurrent_writes() {
    let buffer = Arc::new(RingBuffer::new(10_000, 2 * 1024 * 1024));
    let num_tasks = 8;
    let writes_per_task = 500;
    let mut handles = Vec::new();

    for task_id in 0..num_tasks {
        let buf_clone = buffer.clone();
        handles.push(tokio::spawn(async move {
            for i in 0..writes_per_task {
                buf_clone.push(LogEntry::new(
                    format!("svc_{}", task_id),
                    LogStream::Stdout,
                    format!("Task {} Msg {}", task_id, i),
                ));
            }
        }));
    }

    for h in handles {
        h.await.expect("Task must complete without panicking");
    }

    let total_expected = num_tasks * writes_per_task;
    assert_eq!(buffer.len(), total_expected);
    assert_eq!(buffer.dropped_count(), 0);

    let recent = buffer.snapshot(100);
    assert_eq!(recent.len(), 100);

    // Verify sequences are strictly monotonically increasing
    for window in recent.windows(2) {
        assert!(window[0].sequence < window[1].sequence);
    }
}

#[test]
fn test_ansi_preservation_and_plain_text() {
    let colored_raw = "\x1b[31m[ERROR]\x1b[0m \x1b[1;32mDatabase connection ready\x1b[0m";
    let entry = LogEntry::new("db", LogStream::Stderr, colored_raw);

    // Raw message preserves all ANSI codes verbatim
    assert_eq!(entry.message, colored_raw);

    // Plain text cleanly strips all ANSI codes
    assert_eq!(entry.plain_text(), "[ERROR] Database connection ready");

    // Test various ANSI escape sequences
    assert_eq!(
        strip_ansi("\x1b[38;2;255;100;0mRGB Color\x1b[0m"),
        "RGB Color"
    );
    assert_eq!(
        strip_ansi("\x1b[0mPlain\x1b[1mBold\x1b[4mUnderline\x1b[0m"),
        "PlainBoldUnderline"
    );
    assert_eq!(strip_ansi("\x1b]0;Terminal Title\x07Text"), "Text");
    assert_eq!(
        strip_ansi("No ANSI escape codes here"),
        "No ANSI escape codes here"
    );
}

#[test]
fn test_timestamp_and_formatting() {
    let fixed_time = chrono::DateTime::parse_from_rfc3339("2026-11-07T14:30:15.123Z")
        .unwrap()
        .with_timezone(&Utc);

    let entry = LogEntry::new("api", LogStream::Stdout, "GET /v1/health 200 OK")
        .with_sequence(42)
        .with_timestamp(fixed_time);

    assert_eq!(
        entry.formatted(),
        "[14:30:15.123] [stdout] GET /v1/health 200 OK"
    );
    assert_eq!(
        entry.formatted_with_service(),
        "[14:30:15.123] [api] [stdout] GET /v1/health 200 OK"
    );
}

#[tokio::test]
async fn test_clear_buffer() {
    let buffer = RingBuffer::new(20, 1024 * 1024);
    for i in 0..10 {
        buffer.push(LogEntry::new(
            "svc",
            LogStream::Stdout,
            format!("msg {}", i),
        ));
    }

    assert_eq!(buffer.len(), 10);
    assert!(!buffer.is_empty());

    buffer.clear();

    assert_eq!(buffer.len(), 0);
    assert!(buffer.is_empty());
    assert_eq!(buffer.snapshot(10).len(), 0);
}

#[tokio::test]
async fn test_spawn_stream_reader_integration() {
    let (mut client_tx, server_rx) = tokio::io::duplex(1024);
    let buffer = Arc::new(RingBuffer::new(50, 1024 * 1024));

    let handle = spawn_stream_reader(
        server_rx,
        "stream_svc".to_string(),
        LogStream::Stdout,
        buffer.clone(),
    );

    client_tx.write_all(b"First streamed line\n").await.unwrap();
    client_tx
        .write_all(b"Second streamed line\nThird streamed line\n")
        .await
        .unwrap();
    drop(client_tx); // Signals EOF

    handle.await.unwrap();

    let logs = buffer.snapshot(10);
    assert_eq!(logs.len(), 3);
    assert_eq!(logs[0].message, "First streamed line");
    assert_eq!(logs[1].message, "Second streamed line");
    assert_eq!(logs[2].message, "Third streamed line");
    assert_eq!(logs[0].service_name, "stream_svc");
    assert_eq!(logs[0].stream, LogStream::Stdout);
}
